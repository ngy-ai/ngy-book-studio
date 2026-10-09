//! The library list itself, its sort/view controls and its status bar.

use super::*;
use gpui_kit::component::Selectable as _;

impl EpubReaderApp {
    pub(super) fn set_error(&mut self, text: String, cx: &mut Context<Self>) {
        self.notice = Some(Notice { text, error: true });
        cx.notify();
    }

    pub(super) fn accent_button(&self, id: impl Into<gpui_kit::ElementId>, cx: &App) -> Button {
        Button::new(id)
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(rgb(palette(cx).accent).into())
                    // 与 `theme.primary_foreground` 一致：写死的白色文字压在深色
                    // 皮肤的 `accent`（偏亮的砖红）上只有 3.2:1，读不出来。
                    .foreground(rgb(palette(cx).paper).into())
                    // 0.6 的自定义按钮变体去掉了 `border`：描边不再单独存在，
                    // 边框与 `color` 同色，实心填充的观感不变。
                    .hover(rgb(palette(cx).accent_strong).into())
                    .active(rgb(palette(cx).accent_strong).into()),
            )
            .rounded(px(9.))
    }

    pub(super) fn render_library(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let view = cx.entity().clone();
        let count = self.library.books().len();
        let mode_view = view.clone();
        let search_modes = TabBar::new("library-search-mode")
            .segmented()
            .small()
            .selected_index(search_mode_index(self.search_mode))
            .on_click(move |index, _, cx| {
                mode_view.update(cx, |this, cx| {
                    this.set_search_mode(search_mode_from_index(*index), cx);
                });
            })
            .child(Tab::new().label("关键词"))
            .child(Tab::new().label("语义"))
            .child(Tab::new().label("混合"));

        let header = div()
            .h_flex()
            .justify_between()
            .gap_6()
            .px_7()
            .py_3()
            .border_b_1()
            .border_color(rgb(palette(cx).border))
            .bg(rgb(palette(cx).surface))
            .child(
                div()
                    .h_flex()
                    .flex_none()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size_9()
                            .rounded(px(10.))
                            .bg(rgb(palette(cx).accent))
                            .text_color(white())
                            .child(Icon::new(IconName::BookOpen)),
                    )
                    .child(
                        div()
                            .v_flex()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_lg()
                                    .font_semibold()
                                    .text_color(rgb(palette(cx).ink))
                                    .child("墨页"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(palette(cx).muted))
                                    .child("你的私人图书馆"),
                            ),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .max_w(px(820.))
                    .justify_end()
                    .gap_3()
                    .child(div().flex_none().child(search_modes))
                    .child(
                        div().flex_1().min_w(px(220.)).max_w(px(420.)).child(
                            Input::new(&self.search_input)
                                .prefix(Icon::new(IconName::Search).small())
                                .cleanable(true),
                        ),
                    )
                    .child(
                        Button::new("ai-provider-settings")
                            .ghost()
                            .icon(IconName::Settings2)
                            .label("AI 设置")
                            .dropdown_menu({
                                let view = view.clone();
                                move |menu, _, _| {
                                    let mut menu = menu;
                                    for section in AiSettingsSection::ALL {
                                        let view = view.clone();
                                        menu = menu.item(
                                            PopupMenuItem::new(section.label()).on_click(
                                                move |_, _, cx| {
                                                    view.update(cx, |this, cx| {
                                                        this.open_ai_settings(section, cx)
                                                    });
                                                },
                                            ),
                                        );
                                    }
                                    menu
                                }
                            }),
                    )
                    .child(
                        Button::new("background-jobs")
                            .ghost()
                            .icon(IconName::LoaderCircle)
                            .label("后台任务")
                            .on_click({
                                let view = view.clone();
                                move |_, _, cx| {
                                    view.update(cx, |this, cx| this.open_background_jobs(cx));
                                }
                            }),
                    )
                    .child(
                        Button::new("create-book")
                            .ghost()
                            .icon(IconName::Plus)
                            .label("新建图书")
                            .disabled(self.is_importing)
                            .on_click({
                                let view = view.clone();
                                move |_, window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.open_create_book_dialog(window, cx)
                                    });
                                }
                            }),
                    )
                    .child(
                        self.accent_button("import-book", cx)
                            .icon(IconName::Plus)
                            .label(if self.is_importing {
                                "正在导入…"
                            } else {
                                "导入图书"
                            })
                            .disabled(self.is_importing)
                            .on_click(move |_, window, cx| {
                                view.update(cx, |this, cx| this.import_book(window, cx));
                            }),
                    ),
            );

        let visible = self.visible_book_indices();
        let searching = !self.search_query.is_empty();
        let (heading, subtitle) = if searching {
            let detail = if self.search_loading {
                format!("{}检索中…", search_mode_label(self.search_mode))
            } else if self.search_error.is_some() {
                format!("{}检索失败", search_mode_label(self.search_mode))
            } else {
                format!(
                    "{}检索 · {} 个结果",
                    search_mode_label(self.search_mode),
                    self.search_results.len()
                )
            };
            (
                "当前范围搜索".to_string(),
                format!("“{}” · {detail}", self.search_query),
            )
        } else {
            self.group_heading(visible.len())
        };

        let title = div()
            .h_flex()
            .items_center()
            .justify_between()
            .gap_3()
            .min_h(px(54.))
            .pb_4()
            .border_b_1()
            .border_color(rgb(palette(cx).border))
            .child(
                div()
                    .v_flex()
                    .min_w(px(0.))
                    .gap_0p5()
                    .child(
                        div()
                            .text_2xl()
                            .font_semibold()
                            .text_color(rgb(palette(cx).ink))
                            .child(heading),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(palette(cx).muted))
                            .child(subtitle),
                    ),
            )
            .when(!searching, |this| {
                // Sort and display mode are properties of the shelf, not of a
                // search: results keep their own relevance order and always
                // render as rows.
                this.child(
                    div()
                        .h_flex()
                        .flex_none()
                        .gap_2()
                        .child(self.render_book_sort_button(cx))
                        .child(self.render_book_view_toggle(cx))
                        .when_some(self.group_filter.group_id(), |this, group_id| {
                            this.child(self.render_group_actions(group_id, cx))
                        }),
                )
            });

        let content = if searching {
            self.render_library_search_results(cx)
        } else if count == 0 {
            self.render_empty_library(cx)
        } else if visible.is_empty() {
            self.render_empty_group(cx)
        } else {
            match self.book_view {
                BookViewMode::Cards => self.render_book_grid(&visible, cx),
                BookViewMode::List => self.render_book_list(&visible, cx),
            }
        };

        div()
            .v_flex()
            .size_full()
            .bg(rgb(palette(cx).paper))
            .child(header)
            .child(
                div()
                    .h_flex()
                    .items_start()
                    .flex_1()
                    .min_h(px(0.))
                    .gap_5()
                    .p_5()
                    .child(self.render_group_sidebar(cx))
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .h_full()
                            .min_w(px(0.))
                            .min_h(px(0.))
                            .gap_4()
                            .p_5()
                            .rounded(px(14.))
                            .border_1()
                            .border_color(rgb(palette(cx).border))
                            .bg(rgb(palette(cx).surface))
                            .child(title)
                            .child(content),
                    )
                    .child(self.ai_sidebar.clone()),
            )
            .child(self.render_library_status_bar(visible.len(), cx))
            .when_some(self.notice.as_ref(), |this, notice| {
                this.child(self.render_notice(notice, palette(cx)))
            })
            .into_any_element()
    }

    pub(super) fn render_library_status_bar(
        &self,
        visible: usize,
        cx: &App,
    ) -> gpui_kit::AnyElement {
        let theme = palette(cx);
        let (status_icon, status_text, status_color) = if self.search_loading {
            (IconName::Search, "正在检索…", theme.accent)
        } else if self.search_error.is_some() {
            (IconName::TriangleAlert, "检索失败", theme.danger)
        } else if self.is_importing {
            (IconName::BookOpen, "正在导入图书…", theme.accent)
        } else {
            (IconName::CircleCheck, "书库已就绪", theme.success)
        };
        let scope = if !self.search_query.is_empty() {
            format!(
                "{}检索 · {} 个结果",
                search_mode_label(self.search_mode),
                self.search_results.len()
            )
        } else {
            let label = match &self.group_filter {
                GroupFilter::All => "全部图书".to_string(),
                GroupFilter::Ungrouped => "未分组".to_string(),
                GroupFilter::Group(id) => {
                    let path = self
                        .library
                        .group_path(id)
                        .into_iter()
                        .map(|group| group.name)
                        .collect::<Vec<_>>()
                        .join(" / ");
                    if path.is_empty() {
                        "分组".to_string()
                    } else {
                        path
                    }
                }
            };
            format!("当前：{label} · {visible} 本")
        };

        render_status_bar(
            status_icon,
            status_text.to_string(),
            status_color,
            format!(
                "{} 本图书 · {} 个分组",
                self.library.books().len(),
                self.library.groups().len()
            ),
            scope,
            palette(cx),
        )
    }

    /// The sort menu of the shelf: pick the field, then the direction. Both the
    /// card grid and the list reorder through this one choice.
    pub(super) fn render_book_sort_button(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let view = cx.entity().clone();
        let field = self.book_sort;
        let direction = self.book_sort_direction;
        let ascending = direction.is_ascending();

        Button::new("library-sort")
            .ghost()
            .small()
            .icon(if ascending {
                IconName::SortAscending
            } else {
                IconName::SortDescending
            })
            .label(format!("按{}", field.label()))
            .tooltip("排序方式")
            .debug_selector(|| "library-sort".into())
            .dropdown_menu({
                let menu_view = view.clone();
                move |menu, _, _| {
                    let mut menu = menu;
                    for candidate in [
                        BookSortField::Title,
                        BookSortField::Author,
                        BookSortField::AddedAt,
                    ] {
                        let item_view = menu_view.clone();
                        menu = menu.item(
                            PopupMenuItem::new(candidate.label())
                                .checked(candidate == field)
                                .on_click(move |_, _, cx| {
                                    item_view.update(cx, |this, cx| {
                                        this.set_book_sort_field(candidate, cx)
                                    });
                                }),
                        );
                    }
                    let ascending_view = menu_view.clone();
                    let descending_view = menu_view.clone();
                    menu.separator()
                        .item(
                            PopupMenuItem::new("升序")
                                .icon(Icon::new(IconName::SortAscending))
                                .checked(ascending)
                                .on_click(move |_, _, cx| {
                                    ascending_view.update(cx, |this, cx| {
                                        this.set_book_sort_direction(
                                            BookSortDirection::Ascending,
                                            cx,
                                        )
                                    });
                                }),
                        )
                        .item(
                            PopupMenuItem::new("降序")
                                .icon(Icon::new(IconName::SortDescending))
                                .checked(!ascending)
                                .on_click(move |_, _, cx| {
                                    descending_view.update(cx, |this, cx| {
                                        this.set_book_sort_direction(
                                            BookSortDirection::Descending,
                                            cx,
                                        )
                                    });
                                }),
                        )
                }
            })
            .into_any_element()
    }

    /// Card grid or one-book-per-row list. Built as segmented buttons instead
    /// of a `TabBar`: its tabs do not expose `debug_selector`, and the GUI
    /// smoke runs and GPUI tests locate controls through selectors.
    pub(super) fn render_book_view_toggle(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let view = cx.entity().clone();
        let current = self.book_view;
        let mut tabs = Vec::new();
        for (mode, selector, icon, label, tooltip) in [
            (
                BookViewMode::Cards,
                "library-view-cards",
                IconName::LayoutDashboard,
                "卡片",
                "封面卡片",
            ),
            (
                BookViewMode::List,
                "library-view-list",
                IconName::Menu,
                "列表",
                "一行一本",
            ),
        ] {
            let tab_view = view.clone();
            tabs.push(
                Button::new(selector)
                    .ghost()
                    .small()
                    .icon(icon)
                    .label(label)
                    .tooltip(tooltip)
                    .selected(mode == current)
                    .debug_selector(move || selector.to_string())
                    .on_click(move |_, _, cx| {
                        tab_view.update(cx, |this, cx| this.set_book_view(mode, cx));
                    })
                    .into_any_element(),
            );
        }

        div()
            .h_flex()
            .flex_none()
            .gap_0p5()
            .p_0p5()
            .rounded(px(9.))
            .border_1()
            .border_color(rgb(palette(cx).border))
            .bg(rgb(palette(cx).surface))
            .children(tabs)
            .into_any_element()
    }

    /// Picking a field applies that field's default direction; the direction
    /// items in the menu then toggle it without changing the field.
    pub(super) fn set_book_sort_field(&mut self, field: BookSortField, cx: &mut Context<Self>) {
        if self.book_sort == field {
            return;
        }
        self.book_sort = field;
        self.book_sort_direction = field.default_direction();
        cx.notify();
    }

    pub(super) fn set_book_sort_direction(
        &mut self,
        direction: BookSortDirection,
        cx: &mut Context<Self>,
    ) {
        if self.book_sort_direction == direction {
            return;
        }
        self.book_sort_direction = direction;
        cx.notify();
    }

    pub(super) fn set_book_view(&mut self, view: BookViewMode, cx: &mut Context<Self>) {
        if self.book_view == view {
            return;
        }
        self.book_view = view;
        cx.notify();
    }

    pub(super) fn group_heading(&self, visible: usize) -> (String, String) {
        match &self.group_filter {
            GroupFilter::All => ("我的图书馆".to_string(), format!("{visible} 本图书")),
            GroupFilter::Ungrouped => ("未分组".to_string(), format!("{visible} 本图书")),
            GroupFilter::Group(id) => {
                let path = self.library.group_path(id);
                let name = path
                    .last()
                    .map(|group| group.name.clone())
                    .unwrap_or_else(|| "分组".to_string());
                let breadcrumb = if path.len() > 1 {
                    let names = path
                        .iter()
                        .map(|group| group.name.clone())
                        .collect::<Vec<_>>()
                        .join(" / ");
                    format!("{names} · {visible} 本图书")
                } else {
                    format!("{visible} 本图书")
                };
                (name, breadcrumb)
            }
        }
    }
}
