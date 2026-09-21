//! The translation block table and the block detail modal.

use super::*;

impl BackgroundJobsWindow {
    /// Rows of the translation block inspector: live counters, state filters,
    /// the current page of blocks, and the page controls. Blocks are listed by
    /// ascending task cursor so `#n` matches the `ordinal` in the run log.
    pub(super) fn translation_block_rows(
        &self,
        job: &BackgroundJobSnapshot,
        inspector: Option<&BlockInspector>,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        rows.push(
            div()
                .text_color(rgb(palette(cx).muted))
                .child(
                    "文本块按任务游标顺序排列，序号从 0 开始，与运行日志的 ordinal 一致；预览为原文开头。状态随任务进度实时更新。",
                )
                .into_any_element(),
        );
        if let Some(error) = &self.blocks_error {
            rows.push(
                div()
                    .text_color(rgb(palette(cx).danger))
                    .child(error.clone())
                    .into_any_element(),
            );
            rows.push(
                Button::new("jobs-blocks-reload")
                    .small()
                    .outline()
                    .label("重新加载文本块明细")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.clear_blocks();
                        this.refresh_blocks(window, cx);
                        cx.notify();
                    }))
                    .into_any_element(),
            );
            return rows;
        }
        // The inspector and the block list are built together, so neither can
        // exist without the other.
        let Some((blocks, inspector)) = self.blocks.as_ref().zip(inspector) else {
            rows.push(
                div()
                    .text_color(rgb(palette(cx).muted))
                    .child(if self.blocks_loading {
                        "正在读取文本块明细…"
                    } else {
                        "尚未加载文本块明细。"
                    })
                    .into_any_element(),
            );
            return rows;
        };
        let progress = &inspector.progress;
        if let Some(banner) = failure_banner(job, inspector, palette(cx)) {
            rows.push(banner);
        }
        rows.push(
            div()
                .h_flex()
                .flex_wrap()
                .gap_2()
                .text_color(rgb(palette(cx).ink))
                .child(format!(
                    "共 {} 个文本块 · 已处理 {} · 处理中 {} · 失败 {} · 待处理 {}",
                    progress.total,
                    progress.done,
                    progress.processing.len(),
                    progress.failed,
                    progress.pending
                ))
                .into_any_element(),
        );
        rows.push(
            div()
                .text_color(rgb(palette(cx).muted))
                .child(format!(
                    "目标语言：{}",
                    translation_language_label(&blocks.target_language)
                        .unwrap_or(blocks.target_language.as_str())
                ))
                .into_any_element(),
        );

        let mut filters = div().h_flex().flex_wrap().gap_1();
        for (state, label) in [
            (None, format!("全部 {}", progress.total)),
            (Some(BlockState::Done), format!("已处理 {}", progress.done)),
            (
                Some(BlockState::Processing),
                format!("处理中 {}", progress.processing.len()),
            ),
            (
                Some(BlockState::Failed),
                format!("失败 {}", progress.failed),
            ),
            (
                Some(BlockState::Pending),
                format!("待处理 {}", progress.pending),
            ),
        ] {
            let id = state.map(BlockState::id).unwrap_or("all");
            filters = filters.child(
                Button::new(SharedString::from(format!("jobs-blocks-filter-{id}")))
                    .small()
                    .ghost()
                    .label(label)
                    .when(self.blocks_state == state, |button| {
                        button.custom(
                            ButtonCustomVariant::new(cx)
                                .color(rgb(palette(cx).accent_soft).into())
                                .foreground(rgb(palette(cx).accent_strong).into()),
                        )
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.blocks_state = state;
                        this.blocks_page = 0;
                        this.detail_scroll.set_offset(gpui::point(px(0.), px(0.)));
                        cx.notify();
                    })),
            );
        }
        rows.push(filters.into_any_element());

        // The repair entry point sits in the header, in front of 重新翻译. This
        // line stays here because it is about the list it belongs to: it explains
        // how the two retries differ, so a failed row is not confused with a
        // whole-list re-send.
        if progress.failed > 0 {
            rows.push(
                div()
                    .text_color(rgb(palette(cx).muted))
                    .child(
                        "更换更擅长指令的翻译模型后，可用右上方「重试失败块」重跑全部未翻译的块；\
                         行内「重试」只重跑这一块。",
                    )
                    .into_any_element(),
            );
        }

        let filtered = progress.filtered(self.blocks_state);
        let pages = filtered.div_ceil(BLOCKS_PER_PAGE).max(1);
        let page = self.blocks_page.min(pages - 1);
        let indices = block_page_indices(blocks, &inspector, self.blocks_state, page);
        // Resolved here: a click handler must not borrow the block list, and the
        // anchor page depends on the filter that is active right now.
        let anchor = anchor_page(blocks, &inspector, self.blocks_state).min(pages - 1);
        rows.push(
            div()
                .h_flex()
                .gap_2()
                .text_color(rgb(palette(cx).muted))
                .child(match (indices.first(), indices.last()) {
                    (Some(first), Some(last)) => format!(
                        "显示 #{first}–#{last}（筛选后 {filtered} 块 · 第 {}/{} 页）",
                        page + 1,
                        pages
                    ),
                    _ => format!("筛选后 0 块 · 第 {}/{} 页", page + 1, pages),
                })
                .into_any_element(),
        );
        if indices.is_empty() {
            rows.push(
                div()
                    .py_2()
                    .text_color(rgb(palette(cx).muted))
                    .child("当前筛选下没有文本块。")
                    .into_any_element(),
            );
        }
        for index in indices {
            let Some(block) = blocks.blocks.get(index) else {
                continue;
            };
            let state = inspector.state(index);
            let (foreground, background) = state.colors(palette(cx));
            // Resolved before the row is built: the click handler must not
            // borrow the block list, which lives only for this frame.
            let ordinal = block.ordinal;
            rows.push(
                div()
                    .v_flex()
                    .gap_1()
                    .py_1()
                    .border_b_1()
                    .border_color(rgb(palette(cx).border))
                    .child(
                        div()
                            .h_flex()
                            .items_start()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(52.))
                                    .flex_none()
                                    .text_color(rgb(palette(cx).muted))
                                    .child(format!("#{}", block.ordinal)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .px_2()
                                    .rounded(px(4.))
                                    .bg(background)
                                    .text_color(foreground)
                                    .child(state.label()),
                            )
                            .child(
                                div()
                                    .w(px(128.))
                                    .flex_none()
                                    .truncate()
                                    .text_color(rgb(palette(cx).ink))
                                    .child(block_chapter_label(block)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_color(rgb(palette(cx).muted))
                                    .child(if block.source_preview.is_empty() {
                                        "（无文字内容）".to_string()
                                    } else {
                                        block.source_preview.clone()
                                    }),
                            )
                            // The preview is capped at 96 characters and the list
                            // holds no full text, so inspecting one block is the
                            // only way to see what the worker actually sends.
                            .child(
                                Button::new(SharedString::from(format!(
                                    "jobs-block-view-{}",
                                    block.ordinal
                                )))
                                .debug_selector({
                                    let selector = format!("jobs-block-view-{ordinal}");
                                    move || selector
                                })
                                .small()
                                .ghost()
                                .flex_none()
                                .label("查看")
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.open_block_detail(
                                            ordinal,
                                            state == BlockState::Failed,
                                            window,
                                            cx,
                                        );
                                    },
                                )),
                            )
                            // Only a failed row offers a retry, and it re-sends
                            // exactly this block: the run that skipped it finished
                            // as `Succeeded`, so this button is the way back to one
                            // block after switching to a better model. Other
                            // failed blocks are left alone; the header's
                            // 重试失败块 is the bulk route.
                            .when(state == BlockState::Failed, |row| {
                                row.child(
                                    Button::new(SharedString::from(format!(
                                        "jobs-block-retry-{}",
                                        ordinal
                                    )))
                                    .debug_selector({
                                        let selector = format!("jobs-block-retry-{ordinal}");
                                        move || selector
                                    })
                                    .small()
                                    .flex_none()
                                    .label("重试")
                                    .disabled(self.pending_job_id.is_some())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.retry_translation_blocks(vec![ordinal], window, cx);
                                    })),
                                )
                            }),
                    )
                    // Only failed rows get a second line, so a healthy task keeps
                    // exactly the width the columns had before.
                    .when_some(
                        inspector.reason(index).map(str::to_string),
                        |row, reason| {
                            row.child(
                                div()
                                    .pl(px(60.))
                                    .text_color(rgb(palette(cx).danger))
                                    .child(format!("失败原因：{reason}")),
                            )
                        },
                    )
                    .into_any_element(),
            );
        }

        rows.push(
            div()
                .h_flex()
                .items_center()
                .gap_1()
                .child(
                    Button::new("jobs-blocks-prev")
                        .small()
                        .outline()
                        .label("上一页")
                        .disabled(page == 0)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.blocks_page = this.blocks_page.saturating_sub(1);
                            this.detail_scroll.set_offset(gpui::point(px(0.), px(0.)));
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("jobs-blocks-next")
                        .small()
                        .outline()
                        .label("下一页")
                        .disabled(page + 1 >= pages)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.blocks_page = (this.blocks_page + 1).min(pages - 1);
                            this.detail_scroll.set_offset(gpui::point(px(0.), px(0.)));
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("jobs-blocks-current")
                        .small()
                        .ghost()
                        .label("跳到当前进度")
                        .disabled(progress.total == 0)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.blocks_page = anchor;
                            this.detail_scroll.set_offset(gpui::point(px(0.), px(0.)));
                            cx.notify();
                        })),
                )
                .into_any_element(),
        );
        rows
    }

    /// Debug view of one text block: the identifiers, the full source, and the
    /// exact request body the worker would send. Rendered as an in-window
    /// overlay so it stays attached to the task it describes.
    pub(super) fn render_block_detail_modal(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let state = self.block_detail.as_ref()?;
        let ordinal = state.ordinal;
        let detail = state.detail.as_ref();
        let text = block_detail_text(state, detail);

        let mut tabs = div().h_flex().flex_wrap().gap_1();
        for tab in block_detail_tabs(state.failed) {
            tabs = tabs.child(
                Button::new(SharedString::from(format!(
                    "jobs-block-detail-tab-{}",
                    tab.id()
                )))
                .small()
                .ghost()
                .label(tab.label())
                .when(state.tab == tab, |button| {
                    button.custom(
                        ButtonCustomVariant::new(cx)
                            .color(rgb(palette(cx).accent_soft).into())
                            .foreground(rgb(palette(cx).accent_strong).into()),
                    )
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(state) = this.block_detail.as_mut() {
                        state.tab = tab;
                        state.scroll.set_offset(gpui::point(px(0.), px(0.)));
                    }
                    cx.notify();
                })),
            );
        }

        let copy_view = cx.entity().clone();
        let copy_text = text.clone();
        let title = match detail {
            Some(detail) if !detail.unit_title.is_empty() => format!(
                "文本块 #{ordinal} · {} · 第 {} 章",
                detail.block_id,
                detail.unit_ordinal + 1
            ),
            _ => format!("文本块 #{ordinal}"),
        };

        // Height is resolved here so the scroll container owns a real bound: a
        // naturally sized body would make the overlay grow past the viewport and
        // the body text past the largest texture GPUI can rasterize.
        let available = f32::from(window.viewport_size().height) - 40.;
        let panel_height = if available.is_finite() {
            available.clamp(320., 720.)
        } else {
            640.
        };
        let scroll_handle = state.scroll.clone();
        let mut body = div()
            .id("jobs-block-detail-body")
            .debug_selector(|| "jobs-block-detail-body".into())
            .v_flex()
            .flex_1()
            .min_h(px(0.))
            .gap_2()
            .overflow_y_scroll()
            .track_scroll(&scroll_handle)
            .pr_1();
        if let Some(error) = state.error.as_ref() {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(rgb(palette(cx).danger))
                    .child(error.clone()),
            );
        } else if state.loading {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(rgb(palette(cx).muted))
                    .child("正在读取文本块详情…"),
            );
        } else if state.tab == BlockDetailTab::Response {
            body = body.child(self.render_block_probe(&state.probe, cx));
        } else if let Some(text) = text.as_ref() {
            body = body.child(
                div()
                    .id(SharedString::from(format!(
                        "jobs-block-detail-text-{}",
                        state.tab.id()
                    )))
                    .w_full()
                    .flex_none()
                    .text_xs()
                    .line_height(gpui::relative(1.5))
                    .text_color(rgb(palette(cx).ink))
                    // `whitespace_normal` keeps the JSON indentation while still
                    // breaking long lines, so a body never runs off the panel.
                    .whitespace_normal()
                    .child(text.clone()),
            );
        }

        let close_view = cx.entity().clone();
        let hint = (detail.is_some()).then(|| match state.tab {
            // The one tab whose text is not reconstructible: say where it came
            // from, so a replayed answer is never mistaken for a stored one.
            BlockDetailTab::Response => {
                "模型响应为一次现场重放的原始回答，只显示在这里，不落库、不写译文。"
            }
            _ => "请求 Body 为工作进程构造的完整 chat-completions 请求，仅用于调试，不落库。",
        });
        let footer = div()
            .h_flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette(cx).muted))
                    .children(hint.map(str::to_string)),
            )
            .child(
                div()
                    .h_flex()
                    .flex_none()
                    .gap_2()
                    .child(
                        Button::new("jobs-block-detail-copy")
                            .small()
                            .outline()
                            .icon(IconName::Copy)
                            .label("复制当前页")
                            .disabled(copy_text.is_none())
                            .on_click(move |_, _, cx| {
                                if let Some(text) = &copy_text {
                                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                    copy_view.update(cx, |this, cx| {
                                        this.notice = Some(JobsNotice {
                                            text: "已复制文本块调试信息。".into(),
                                            error: false,
                                        });
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("jobs-block-detail-close")
                            .small()
                            .primary()
                            .label("关闭")
                            .on_click(move |_, _, cx| {
                                close_view.update(cx, |this, cx| this.close_block_detail(cx));
                            }),
                    ),
            );

        Some(
            div()
                .id("jobs-block-detail-overlay")
                .absolute()
                .inset_0()
                // Painting an overlay does not remove the hitboxes underneath it;
                // make the panel a real mouse barrier so a dismissal cannot also
                // activate a block row behind it.
                .occlude()
                .bg(palette(cx).scrim_with_alpha(0.5))
                .v_flex()
                .items_center()
                .justify_center()
                .on_any_mouse_down(cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if event.button == gpui::MouseButton::Left {
                        this.close_block_detail(cx);
                    }
                }))
                .child(
                    div()
                        .id("jobs-block-detail-panel")
                        .debug_selector(|| "jobs-block-detail-panel".into())
                        .occlude()
                        .v_flex()
                        .w(px(720.))
                        .h(px(panel_height))
                        .max_w(gpui::relative(0.92))
                        .p_5()
                        .gap_3()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(rgb(palette(cx).border))
                        .bg(rgb(palette(cx).surface))
                        .shadow_lg()
                        .child(
                            div()
                                .v_flex()
                                .flex_none()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_semibold()
                                        .text_color(rgb(palette(cx).ink))
                                        .truncate()
                                        .child(title),
                                )
                                .child(
                                    div().text_xs().text_color(rgb(palette(cx).muted)).truncate().child(
                                        detail
                                            .map(|detail| {
                                                // A task that never ran records no
                                                // model, so the header must not
                                                // open on a leading separator.
                                                let model = match detail.request.model.trim() {
                                                    "" => "未记录模型".to_string(),
                                                    model => model.to_string(),
                                                };
                                                format!(
                                                    "{model} · 原文 {} 字符 · {} 个片段 · 单元 {}",
                                                    detail.source_text.chars().count(),
                                                    detail.segments.len(),
                                                    detail.unit_id
                                                )
                                            })
                                            .unwrap_or_else(|| {
                                                "该文本块尚无可用详情。".to_string()
                                            }),
                                    ),
                                ),
                        )
                        .child(tabs)
                        .child(div().flex_none().h(px(1.)).w_full().bg(rgb(palette(cx).border)))
                        .child(body)
                        .child(footer),
                )
                .into_any_element(),
        )
    }
}
