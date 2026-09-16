use super::ai_sidebar::markdown::assistant_display_markdown;
use super::*;

use gpui::{ClipboardItem, Pixels, ScrollHandle};
use gpui_component::text::{TextView, TextViewStyle};
use ngy_book_studio::annotations::{AnnotationKind, AnnotationOverview};
use std::collections::HashMap;

const NOTES_PER_PAGE: usize = 30;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum NoteFilter {
    #[default]
    All,
    Marks,
    Human,
    Ai,
}

impl NoteFilter {
    const ALL: [Self; 4] = [Self::All, Self::Marks, Self::Human, Self::Ai];

    fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Marks => "划线",
            Self::Human => "人工想法",
            Self::Ai => "AI 想法",
        }
    }

    fn includes(self, kind: AnnotationKind) -> bool {
        match self {
            Self::All => true,
            Self::Marks => kind.is_mark(),
            Self::Human => kind == AnnotationKind::HumanComment,
            Self::Ai => kind == AnnotationKind::AiComment,
        }
    }
}

struct NoteCard {
    index: usize,
    quote_html: SharedString,
    comment_markdown: Option<Arc<ThoughtDisplay>>,
}

#[derive(Debug, PartialEq, Eq)]
struct ThoughtDisplay {
    markdown: SharedString,
    parts: Vec<ThoughtPart>,
}

#[derive(Debug, PartialEq, Eq)]
enum ThoughtPart {
    Markdown(SharedString),
    Table {
        rows: Vec<Vec<SharedString>>,
        nested: bool,
    },
}

impl ThoughtDisplay {
    fn new(markdown: String) -> Self {
        use ::markdown::{ParseOptions, mdast::Node, to_mdast};

        let mut parts = Vec::new();
        if let Ok(tree) = to_mdast(&markdown, &ParseOptions::gfm()) {
            let mut cursor = 0;
            for child in tree.children().into_iter().flatten() {
                if let Node::Table(table) = child {
                    if let Some(position) = child.position()
                        && let Some(before) = markdown.get(cursor..position.start.offset)
                    {
                        push_markdown_part(&mut parts, before);
                        parts.push(ThoughtPart::Table {
                            rows: table_rows(table, &markdown),
                            nested: false,
                        });
                        cursor = position.end.offset;
                    }
                } else {
                    let mut nested = Vec::new();
                    let mut pending = vec![child];
                    while let Some(node) = pending.pop() {
                        if let Node::Table(table) = node {
                            nested.push(table_rows(table, &markdown));
                        } else if let Some(children) = node.children() {
                            pending.extend(children.iter().rev());
                        }
                    }
                    // The component can still render the surrounding list or
                    // quote. Add complete native tables immediately after it,
                    // so nested cell content never depends on its truncation.
                    if !nested.is_empty()
                        && let Some(position) = child.position()
                        && let Some(before) = markdown.get(cursor..position.end.offset)
                    {
                        push_markdown_part(&mut parts, before);
                        for rows in nested {
                            parts.push(ThoughtPart::Table { rows, nested: true });
                        }
                        cursor = position.end.offset;
                    }
                }
            }
            if let Some(rest) = markdown.get(cursor..) {
                push_markdown_part(&mut parts, rest);
            }
        }
        Self {
            markdown: markdown.into(),
            parts,
        }
    }
}

fn push_markdown_part(parts: &mut Vec<ThoughtPart>, source: &str) {
    if !source.trim().is_empty() {
        parts.push(ThoughtPart::Markdown(source.to_owned().into()));
    }
}

fn table_rows(table: &::markdown::mdast::Table, source: &str) -> Vec<Vec<SharedString>> {
    table
        .children
        .iter()
        .map(|row| {
            row.children()
                .into_iter()
                .flatten()
                .map(|cell| {
                    // Source ranges belong to the already sanitized Markdown.
                    // Preserve inline emphasis/code without interpreting a row
                    // as a table again. Positions are supplied by this parser.
                    cell.position()
                        .and_then(|position| source.get(position.start.offset..position.end.offset))
                        .map(table_cell_markdown)
                        .unwrap_or_else(|| literal_cell_text(cell))
                        .into()
                })
                .collect()
        })
        .collect()
}

fn table_cell_markdown(source: &str) -> String {
    // mdast's TableCell range includes its leading separator and may include
    // the final row separator. Remove only those structural pipes; escaped
    // pipes and inline code inside the cell remain part of its Markdown.
    let source = source.trim();
    let source = source.strip_prefix('|').unwrap_or(source).trim_start();
    let source = if let Some(without_pipe) = source.strip_suffix('|') {
        let backslashes = without_pipe
            .chars()
            .rev()
            .take_while(|ch| *ch == '\\')
            .count();
        if backslashes % 2 == 0 {
            without_pipe
        } else {
            source
        }
    } else {
        source
    };
    source.trim_end().to_owned()
}

fn literal_cell_text(node: &::markdown::mdast::Node) -> String {
    use std::fmt::Write as _;

    let mut plain = String::new();
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if let Some(children) = node.children() {
            pending.extend(children.iter().rev());
        } else {
            plain.push_str(&node.to_string());
        }
    }
    let mut escaped = String::with_capacity(plain.len());
    for ch in plain.chars() {
        if ch.is_ascii_punctuation() {
            write!(&mut escaped, "&#{};", ch as u32).expect("String writes cannot fail");
        } else {
            escaped.push(ch);
        }
    }
    escaped
}

struct PreparedNotes {
    notes: Vec<AnnotationOverview>,
    comment_markdown: HashMap<String, Arc<ThoughtDisplay>>,
}

impl PreparedNotes {
    // Run after the library read on the background worker. Sanitizing a large
    // saved AI thought requires Markdown parsing, so it must not run in a
    // GPUI callback when loading, searching, or moving between pages.
    fn new(notes: Vec<AnnotationOverview>) -> Self {
        let comment_markdown = notes
            .iter()
            .filter_map(|note| {
                note.annotation.comment.as_deref().map(|comment| {
                    (
                        note.annotation.id.clone(),
                        Arc::new(ThoughtDisplay::new(assistant_display_markdown(comment))),
                    )
                })
            })
            .collect();
        Self {
            notes,
            comment_markdown,
        }
    }
}

#[derive(Default)]
struct NoteListing {
    notes: Vec<AnnotationOverview>,
    filtered: Vec<usize>,
    page: usize,
    cards: Vec<NoteCard>,
    filter: NoteFilter,
    query: String,
    book_count: usize,
    comment_markdown: HashMap<String, Arc<ThoughtDisplay>>,
}

impl NoteListing {
    #[cfg(test)]
    fn replace(&mut self, notes: Vec<AnnotationOverview>) {
        self.replace_prepared(PreparedNotes::new(notes));
    }

    fn replace_prepared(&mut self, prepared: PreparedNotes) {
        let PreparedNotes {
            mut notes,
            comment_markdown,
        } = prepared;
        notes.sort_by(|left, right| {
            right
                .annotation
                .updated_at
                .cmp(&left.annotation.updated_at)
                .then_with(|| right.annotation.created_at.cmp(&left.annotation.created_at))
                .then_with(|| left.annotation.id.cmp(&right.annotation.id))
        });
        self.book_count = notes
            .iter()
            .map(|note| &note.annotation.book_id)
            .collect::<HashSet<_>>()
            .len();
        self.notes = notes;
        self.comment_markdown = comment_markdown;
        self.apply_filter();
    }

    fn apply_filter(&mut self) {
        let query = self.query.trim().to_lowercase();
        self.filtered = self
            .notes
            .iter()
            .enumerate()
            .filter(|(_, note)| {
                self.filter.includes(note.annotation.kind) && note_matches(note, &query)
            })
            .map(|(index, _)| index)
            .collect();
        self.page = 0;
        self.prepare_page();
    }

    fn page_count(&self) -> usize {
        self.filtered.len().div_ceil(NOTES_PER_PAGE).max(1)
    }

    fn set_page(&mut self, page: usize) {
        self.page = page.min(self.page_count() - 1);
        self.prepare_page();
    }

    // Escape only the visible quotes; thoughts use the projection prepared on
    // the background worker. Page/filter changes only clone those SharedStrings.
    fn prepare_page(&mut self) {
        self.cards = self
            .filtered
            .iter()
            .skip(self.page * NOTES_PER_PAGE)
            .take(NOTES_PER_PAGE)
            .map(|&index| {
                let note = &self.notes[index].annotation;
                NoteCard {
                    index,
                    quote_html: plain_text_html(&note.anchor.quote).into(),
                    comment_markdown: self.comment_markdown.get(&note.id).cloned(),
                }
            })
            .collect();
    }
}

fn note_matches(note: &AnnotationOverview, query: &str) -> bool {
    query.is_empty()
        || [
            note.book_title.as_str(),
            note.chapter_title.as_deref().unwrap_or_default(),
            note.annotation.anchor.quote.as_str(),
            note.annotation.comment.as_deref().unwrap_or_default(),
        ]
        .iter()
        .any(|text| text.to_lowercase().contains(query))
}

fn plain_text_html(text: &str) -> String {
    let mut html = String::with_capacity(text.len().saturating_add(16));
    html.push_str("<p>");
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '&' => html.push_str("&amp;"),
            '<' => html.push_str("&lt;"),
            '>' => html.push_str("&gt;"),
            '"' => html.push_str("&quot;"),
            '\'' => html.push_str("&#39;"),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                html.push_str("</p><p>");
            }
            '\n' => html.push_str("</p><p>"),
            _ => html.push(character),
        }
    }
    html.push_str("</p>");
    html
}

fn kind_label(kind: AnnotationKind) -> &'static str {
    match kind {
        AnnotationKind::Highlight => "马克笔",
        AnnotationKind::Wavy => "波浪线",
        AnnotationKind::Underline => "直线",
        AnnotationKind::HumanComment => "人工想法",
        AnnotationKind::AiComment => "AI 想法",
    }
}

fn chapter_label(note: &AnnotationOverview) -> String {
    match note.chapter_index {
        None => "章节已移除".into(),
        Some(index) => note
            .chapter_title
            .as_ref()
            .filter(|title| !title.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| format!("第 {} 章", index + 1)),
    }
}

fn copy_text(note: &AnnotationOverview) -> String {
    let mut text = format!(
        "《{}》 · {}\n{}\n{}",
        note.book_title,
        chapter_label(note),
        kind_label(note.annotation.kind),
        note.annotation.anchor.quote
    );
    if let Some(comment) = &note.annotation.comment {
        text.push_str("\n\n");
        text.push_str(comment);
    }
    text
}

struct NotesWindow {
    services: Arc<AppServices>,
    library_view: Entity<EpubReaderApp>,
    book: Option<(String, String)>,
    listing: NoteListing,
    search: Entity<InputState>,
    _search_subscription: Subscription,
    scroll: ScrollHandle,
    generation: u64,
    loading: bool,
    loaded: bool,
    error: Option<String>,
    copied_note: Option<String>,
    opening_note: Option<String>,
}

impl NotesWindow {
    fn new(
        services: Arc<AppServices>,
        library_view: Entity<EpubReaderApp>,
        book: Option<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索书名、章节、引文或想法…"));
        let subscription = cx.subscribe_in(&search, window, |this, _, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.listing.query = this.search.read(cx).value().to_string();
                this.listing.apply_filter();
                this.reset_scroll();
                cx.notify();
            }
        });
        Self {
            services,
            library_view,
            book,
            listing: NoteListing::default(),
            search,
            _search_subscription: subscription,
            scroll: ScrollHandle::default(),
            generation: 0,
            loading: false,
            loaded: false,
            error: None,
            copied_note: None,
            opening_note: None,
        }
    }

    fn reset_scroll(&mut self) {
        self.scroll.set_offset(gpui::point(px(0.), px(0.)));
        self.copied_note = None;
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.loading = true;
        self.error = None;
        let book_id = self.book.as_ref().map(|(id, _)| id.clone());
        let task = self.services.spawn_library_read(move |library| {
            let notes = library.annotation_overview(book_id.as_deref())?;
            Ok(PreparedNotes::new(notes))
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(Ok(notes)) => {
                        this.listing.replace_prepared(notes);
                        this.loaded = true;
                        this.reset_scroll();
                    }
                    Ok(Err(error)) => {
                        this.error = Some(format!("无法读取笔记：{error:#}。请重试刷新。"));
                    }
                    Err(error) => {
                        this.error = Some(format!("笔记读取任务已停止：{error}。请重试刷新。"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_annotation(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.opening_note.is_some() {
            return;
        }
        let Some(note) = self
            .listing
            .notes
            .iter()
            .find(|note| note.annotation.id == id)
        else {
            return;
        };
        let book_id = note.annotation.book_id.clone();
        let unit_id = note.annotation.content_unit_id.clone();
        let id = id.to_owned();
        self.opening_note = Some(id.clone());
        self.error = None;
        let task = self.services.spawn_library_read(move |library| {
            let current = library
                .list_annotations(&book_id, Some(&unit_id))?
                .into_iter()
                .find(|note| note.id == id)
                .context("这条笔记已删除，请刷新列表")?;
            anyhow::ensure!(!current.stale, "笔记位置已失效，请刷新列表查看当前状态");
            Ok(current)
        });
        cx.spawn_in(window, async move |view, cx| {
            let outcome = task.await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.opening_note = None;
                    match outcome {
                        Ok(Ok(annotation)) => {
                            let error_view = cx.entity().downgrade();
                            this.library_view.update(cx, |library, cx| {
                                library.open_book_at_annotation(
                                    annotation,
                                    move |message, cx| {
                                        let _ = error_view.update(cx, |this, cx| {
                                            this.error = Some(message);
                                            cx.notify();
                                        });
                                    },
                                    window,
                                    cx,
                                );
                            });
                        }
                        Ok(Err(error)) => {
                            this.error = Some(format!("无法打开笔记章节：{error:#}"));
                        }
                        Err(error) => {
                            this.error = Some(format!("笔记定位任务已停止：{error}"));
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
        cx.notify();
    }

    #[inline(never)]
    fn render_header(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let title = if self.book.is_some() {
            "本书笔记"
        } else {
            "全部笔记"
        };
        let subtitle = self
            .book
            .as_ref()
            .map(|(_, title)| format!("《{title}》"))
            .unwrap_or_else(|| "汇集所有图书的划线、人工想法与 AI 想法".into());
        let view = cx.entity();
        div()
            .h_flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_4()
            .px_6()
            .py_4()
            .bg(rgb(SURFACE))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .v_flex()
                    .min_w(px(0.))
                    .gap_1()
                    .child(div().text_lg().font_semibold().child(title))
                    .child(div().text_sm().text_color(rgb(MUTED)).child(subtitle)),
            )
            .child(
                Button::new("notes-refresh")
                    .outline()
                    .icon(IconName::Redo2)
                    .label(if self.loading {
                        "正在刷新…"
                    } else {
                        "刷新"
                    })
                    .disabled(self.loading)
                    .on_click(move |_, window, cx| {
                        view.update(cx, |this, cx| this.refresh(window, cx));
                    }),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn render_filters(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut filters = div().h_flex().gap_2();
        for (index, filter) in NoteFilter::ALL.into_iter().enumerate() {
            let view = cx.entity();
            filters = filters.child(
                Button::new(("notes-filter", index))
                    .small()
                    .label(filter.label())
                    .when(self.listing.filter == filter, |button| button.primary())
                    .when(self.listing.filter != filter, |button| button.ghost())
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.listing.filter = filter;
                            this.listing.apply_filter();
                            this.reset_scroll();
                            cx.notify();
                        });
                    }),
            );
        }
        div()
            .v_flex()
            .flex_none()
            .gap_3()
            .px_6()
            .py_3()
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(Input::new(&self.search).w_full())
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(filters)
                    .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                        "{} 条笔记 · {} 本图书",
                        self.listing.notes.len(),
                        self.listing.book_count
                    ))),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn render_card(
        &self,
        card: &NoteCard,
        width: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let note = &self.listing.notes[card.index];
        let annotation = &note.annotation;
        let id = annotation.id.clone();
        let locate_id = id.clone();
        let locate_view = cx.entity();
        let copy_id = id.clone();
        let view = cx.entity();
        let mut content = div().v_flex().gap_3().child(
            div()
                .border_l_2()
                .border_color(rgb(ACCENT_SOFT))
                .pl_3()
                .text_color(rgb(MUTED))
                .child(selectable_note_text(
                    format!("note-{id}-quote"),
                    card.quote_html.clone(),
                    (width - px(14.)).max(px(0.)),
                    window,
                    cx,
                )),
        );
        if let Some(comment_markdown) = &card.comment_markdown {
            content = content.child(render_note_thought(
                format!("note-{id}-thought-markdown"),
                comment_markdown.clone(),
                width,
                window,
                cx,
            ));
        }
        div()
            .id(SharedString::from(format!("note-card-{id}")))
            .v_flex()
            .flex_shrink_0()
            .gap_3()
            .p_4()
            .rounded(px(10.))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(
                div()
                    .h_flex()
                    .justify_between()
                    .items_start()
                    .gap_3()
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .min_w(px(0.))
                            .gap_1()
                            .child(
                                div()
                                    .font_semibold()
                                    .child(format!("《{}》", note.book_title)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child(chapter_label(note)),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .px_2()
                            .py_1()
                            .rounded(px(5.))
                            .bg(rgb(ACCENT_SOFT))
                            .text_color(rgb(ACCENT_DARK))
                            .child(kind_label(annotation.kind)),
                    ),
            )
            .child(content)
            .when(annotation.stale, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child("笔记位置已失效，保留的引文和想法仍可查看、复制。"),
                )
            })
            .child(
                div()
                    .h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new(SharedString::from(format!("note-copy-{id}")))
                            .small()
                            .ghost()
                            .icon(IconName::Copy)
                            .label(if self.copied_note.as_ref() == Some(&id) {
                                "已复制"
                            } else {
                                "复制笔记"
                            })
                            .on_click(move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    if let Some(note) = this
                                        .listing
                                        .notes
                                        .iter()
                                        .find(|note| note.annotation.id == copy_id)
                                    {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            copy_text(note),
                                        ));
                                        this.copied_note = Some(copy_id.clone());
                                        cx.notify();
                                    }
                                });
                            }),
                    )
                    .child(
                        Button::new(SharedString::from(format!("note-open-{id}")))
                            .small()
                            .outline()
                            .label(if self.opening_note.as_ref() == Some(&id) {
                                "正在打开…"
                            } else {
                                "打开章节"
                            })
                            .disabled(
                                annotation.stale
                                    || note.chapter_index.is_none()
                                    || self.opening_note.is_some(),
                            )
                            .on_click(move |_, window, cx| {
                                locate_view.update(cx, |this, cx| {
                                    this.open_annotation(&locate_id, window, cx);
                                });
                            }),
                    ),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn render_footer(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let previous = cx.entity();
        let next = cx.entity();
        div()
            .h_flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_4()
            .px_6()
            .py_3()
            .bg(rgb(SURFACE))
            .border_t_1()
            .border_color(rgb(BORDER))
            .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                "找到 {} 条 · 最近更新在前",
                self.listing.filtered.len()
            )))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .child(
                        Button::new("notes-previous")
                            .small()
                            .outline()
                            .label("上一页")
                            .disabled(self.listing.page == 0)
                            .on_click(move |_, _, cx| {
                                previous.update(cx, |this, cx| {
                                    this.listing.set_page(this.listing.page.saturating_sub(1));
                                    this.reset_scroll();
                                    cx.notify();
                                });
                            }),
                    )
                    .child(div().text_sm().child(format!(
                        "{} / {}",
                        self.listing.page + 1,
                        self.listing.page_count()
                    )))
                    .child(
                        Button::new("notes-next")
                            .small()
                            .outline()
                            .label("下一页")
                            .disabled(self.listing.page + 1 >= self.listing.page_count())
                            .on_click(move |_, _, cx| {
                                next.update(cx, |this, cx| {
                                    this.listing.set_page(this.listing.page + 1);
                                    this.reset_scroll();
                                    cx.notify();
                                });
                            }),
                    ),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn render_list(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        // Explicit width avoids TextView's clipped short rows on Windows. The
        // card sits inside 48px outer padding and 34px card padding/borders.
        let width = (window.viewport_size().width - px(84.)).max(px(0.));
        let mut list = div()
            .id("notes-scroll")
            .v_flex()
            .size_full()
            .gap_4()
            .p_6()
            .overflow_y_scroll()
            .track_scroll(&self.scroll);
        if self.listing.cards.is_empty() {
            let message = if self.loading && !self.loaded {
                "正在读取笔记…"
            } else if self.error.is_some() && !self.loaded {
                "读取笔记失败，请点击刷新重试。"
            } else if !self.listing.notes.is_empty() {
                "没有符合搜索或类型条件的笔记。"
            } else if self.book.is_some() {
                "这本书还没有笔记。阅读时选择正文即可划线或写想法。"
            } else {
                "还没有笔记。阅读时选择正文即可划线或写想法。"
            };
            list = list.child(
                div()
                    .v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .text_color(rgb(MUTED))
                    .child(Icon::new(IconName::Inbox))
                    .child(message),
            );
        } else {
            for card in &self.listing.cards {
                list = list.child(self.render_card(card, width, window, cx));
            }
        }
        div()
            .relative()
            .flex_1()
            .min_h(px(0.))
            .child(list)
            .vertical_scrollbar(&self.scroll)
            .into_any_element()
    }
}

#[inline(never)]
fn selectable_note_text(
    id: String,
    html: SharedString,
    width: Pixels,
    window: &mut Window,
    cx: &mut App,
) -> TextView {
    TextView::html(SharedString::from(id), html, window, cx)
        .style(TextViewStyle::default().paragraph_gap(gpui::rems(0.4)))
        .selectable(true)
        .scrollable(false)
        .w(width)
        .h_auto()
        .flex_shrink_0()
}

#[inline(never)]
fn selectable_note_thought(
    id: String,
    markdown: SharedString,
    width: Pixels,
    window: &mut Window,
    cx: &mut App,
) -> TextView {
    TextView::markdown(SharedString::from(id), markdown, window, cx)
        .style(TextViewStyle::default().paragraph_gap(gpui::rems(0.4)))
        .selectable(true)
        .scrollable(false)
        .w(width)
        .h_auto()
        .flex_shrink_0()
}

#[inline(never)]
fn render_note_thought(
    id: String,
    display: Arc<ThoughtDisplay>,
    width: Pixels,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyElement {
    if display.parts.is_empty() {
        return selectable_note_thought(id, display.markdown.clone(), width, window, cx)
            .into_any_element();
    }
    let mut body = div().v_flex().w(width).gap_2();
    for (index, part) in display.parts.iter().enumerate() {
        let part_id = format!("{id}-part-{index}");
        match part {
            ThoughtPart::Markdown(markdown) => {
                body = body.child(selectable_note_thought(
                    part_id,
                    markdown.clone(),
                    width,
                    window,
                    cx,
                ));
            }
            ThoughtPart::Table { rows, nested } => {
                if *nested {
                    body = body.child(div().text_xs().text_color(rgb(MUTED)).child("完整表格"));
                }
                body = body.child(render_note_table(&part_id, rows, width, window, cx));
            }
        }
    }
    body.into_any_element()
}

#[inline(never)]
fn render_note_table(
    id: &str,
    rows: &[Vec<SharedString>],
    width: Pixels,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyElement {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let column_width = ((width - px(2.)) / columns as f32).max(px(180.));
    let table_width = column_width * columns as f32;
    let scroll = window
        .use_keyed_state(
            SharedString::from(format!("{id}-scroll-state")),
            cx,
            |_, _| ScrollHandle::default(),
        )
        .read(cx)
        .clone();
    let mut table = div()
        .v_flex()
        .w(table_width + px(2.))
        .flex_shrink_0()
        .border_1()
        .border_color(rgb(BORDER));
    for (row_index, row) in rows.iter().enumerate() {
        let mut line = div()
            .flex()
            .flex_row()
            .w(table_width)
            .flex_shrink_0()
            .when(row_index == 0, |line| line.bg(rgb(SIDEBAR)).font_semibold())
            .when(row_index + 1 < rows.len(), |line| {
                line.border_b_1().border_color(rgb(BORDER))
            });
        for column in 0..columns {
            let cell_id = format!("{id}-cell-{row_index}-{column}");
            let source = row.get(column).cloned().unwrap_or_else(|| "".into());
            let cell = div()
                .id(SharedString::from(cell_id.clone()))
                .debug_selector({
                    let cell_id = cell_id.clone();
                    move || cell_id.clone()
                })
                .w(column_width)
                .min_h(px(40.))
                .flex_shrink_0()
                .p_3()
                .when(column + 1 < columns, |cell| {
                    cell.border_r_1().border_color(rgb(BORDER))
                })
                .child(selectable_note_thought(
                    format!("{cell_id}-text"),
                    source,
                    (column_width - px(26.)).max(px(0.)),
                    window,
                    cx,
                ));
            line = line.child(cell);
        }
        table = table.child(line);
    }
    // Keep the natural table height. The component's overflow_x_scrollbar()
    // wrapper uses size_full(), so attach the scrollbar to our own viewport.
    div()
        .relative()
        .w(width)
        .flex_shrink_0()
        .child(
            div()
                .id(SharedString::from(format!("{id}-viewport")))
                .w(width)
                .pb_3()
                .overflow_x_scroll()
                .track_scroll(&scroll)
                .child(table),
        )
        .horizontal_scrollbar(&scroll)
        .into_any_element()
}

impl Render for NotesWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .v_flex()
            .size_full()
            .text_color(rgb(INK))
            .text_sm()
            .bg(rgb(PAPER))
            .child(self.render_header(cx))
            .child(self.render_filters(cx))
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .flex_none()
                        .px_6()
                        .py_2()
                        .text_sm()
                        .text_color(rgb(DANGER))
                        .child(error),
                )
            })
            .child(self.render_list(window, cx))
            .child(self.render_footer(cx))
    }
}

pub(super) fn open_notes_window(
    services: Arc<AppServices>,
    library_view: Entity<EpubReaderApp>,
    book: Option<(String, String)>,
    cx: &mut App,
) -> Result<()> {
    if application_is_exiting(cx) {
        return Ok(());
    }
    let title = book
        .as_ref()
        .map(|(_, title)| format!("《{title}》 · 本书笔记"))
        .unwrap_or_else(|| "墨页 · 全部笔记".into());
    let book_id = book.as_ref().map(|(id, _)| id.clone());
    // One notes window per scope: "全部笔记" is global, "本书笔记" is per book.
    let key = book_id
        .as_ref()
        .map(|id| singleton_window_key("notes", id))
        .unwrap_or_else(|| singleton_window_key("notes", "all"));
    match reserve_singleton_window(&key, cx) {
        SingletonWindowReservation::Activate(handle) => {
            activate_singleton_window(handle, cx);
            return Ok(());
        }
        SingletonWindowReservation::InFlight => return Ok(()),
        SingletonWindowReservation::Reserved => {}
    }
    let bounds = Bounds::centered(None, size(px(860.), px(760.)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(680.), px(500.))),
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                ..Default::default()
            }),
            app_id: Some("dev.ngy.book-studio.notes".into()),
            ..Default::default()
        },
        move |window, cx| {
            let notes = cx.new(|cx| NotesWindow::new(services, library_view, book, window, cx));
            notes.update(cx, |this, cx| this.refresh(window, cx));
            on_window_close(window, cx, |_, _| true);
            if let Some(book_id) = book_id {
                register_book_window(
                    book_id,
                    window,
                    |window, cx| remove_window_after_current_frame(window, cx, None),
                    cx,
                );
            }
            cx.new(|cx| Root::new(notes, window, cx))
        },
    );
    match opened {
        Ok(handle) => {
            complete_singleton_window(&key, handle.into(), cx);
            Ok(())
        }
        Err(error) => {
            release_singleton_window(&key, cx);
            Err(error).context("无法创建笔记窗口")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        Modifiers, MouseButton, ScrollDelta, ScrollWheelEvent, TestAppContext, VisualTestContext,
        point,
    };
    use ngy_book_studio::annotations::{Annotation, TextAnchor};

    fn note(id: usize, book: &str, kind: AnnotationKind) -> AnnotationOverview {
        AnnotationOverview {
            annotation: Annotation {
                id: format!("note-{id:03}"),
                book_id: book.into(),
                content_unit_id: format!("unit-{id}"),
                document_revision: 1,
                unit_revision: 1,
                anchor: TextAnchor {
                    quote: format!("引文{id}"),
                    start: 0,
                    end: 1,
                },
                kind,
                comment: kind.is_comment().then(|| format!("想法{id}")),
                created_at: id as u64,
                updated_at: id as u64,
                stale: false,
            },
            book_title: book.into(),
            chapter_title: Some(format!("章节{id}")),
            chapter_index: Some(id),
        }
    }

    #[test]
    fn filters_combine_type_with_book_chapter_quote_and_thought_search() {
        let mut listing = NoteListing::default();
        listing.replace(vec![
            note(1, "Rust 学习", AnnotationKind::Highlight),
            note(2, "Rust 学习", AnnotationKind::HumanComment),
            note(3, "Agent 实践", AnnotationKind::AiComment),
            note(4, "Agent 实践", AnnotationKind::Wavy),
            note(5, "Agent 实践", AnnotationKind::Underline),
        ]);
        assert_eq!(listing.book_count, 2);
        for query in ["rust", "  RUST  ", "章节2", "引文2", "想法2"] {
            listing.query = query.into();
            listing.filter = NoteFilter::Human;
            listing.apply_filter();
            assert_eq!(listing.filtered.len(), 1, "{query}");
            assert_eq!(listing.notes[listing.filtered[0]].annotation.id, "note-002");
        }
        listing.query.clear();
        listing.filter = NoteFilter::Marks;
        listing.apply_filter();
        assert_eq!(listing.filtered.len(), 3);
        listing.filter = NoteFilter::Ai;
        listing.apply_filter();
        assert_eq!(listing.filtered.len(), 1);
        listing.query = "Rust".into();
        listing.apply_filter();
        assert!(listing.cards.is_empty());
    }

    #[test]
    fn pages_cover_every_result_and_reset_after_filter_or_refresh() {
        let mut listing = NoteListing::default();
        listing.replace(
            (0..65)
                .map(|id| note(id, "书", AnnotationKind::Highlight))
                .collect(),
        );
        assert_eq!(listing.page_count(), 3);
        assert_eq!(listing.cards.len(), 30);
        assert_eq!(
            listing.notes[listing.cards[0].index].annotation.updated_at,
            64
        );
        let mut visited = HashSet::new();
        for page in 0..3 {
            listing.set_page(page);
            for card in &listing.cards {
                assert!(visited.insert(listing.notes[card.index].annotation.id.clone()));
            }
        }
        assert_eq!(visited.len(), 65);
        assert_eq!(listing.cards.len(), 5);
        listing.set_page(usize::MAX);
        assert_eq!(listing.page, 2);
        listing.query = "引文64".into();
        listing.apply_filter();
        assert_eq!(listing.page, 0);
        assert_eq!(listing.cards.len(), 1);
        listing.replace(Vec::new());
        assert_eq!(listing.page, 0);
        assert_eq!(listing.page_count(), 1);
        assert_eq!(listing.book_count, 0);
        assert!(listing.cards.is_empty());
    }

    #[test]
    fn stale_notes_keep_complete_original_text_and_never_gain_a_chapter() {
        let mut old_note = note(0, "书", AnnotationKind::AiComment);
        old_note.annotation.stale = true;
        old_note.chapter_index = None;
        old_note.chapter_title = None;
        let thought = format!(
            "<script>alert('x')</script>\r\n{}\n末尾",
            "长想法".repeat(12_000)
        );
        old_note.annotation.comment = Some(thought.clone());
        let copied = copy_text(&old_note);
        assert!(copied.ends_with(&thought));
        assert_eq!(chapter_label(&old_note), "章节已移除");
        let mut listing = NoteListing::default();
        listing.replace(vec![old_note]);
        let display = &listing.cards[0].comment_markdown.as_ref().unwrap().markdown;
        assert!(display.starts_with("&#60;script&#62;"));
        assert!(!display.contains("<script>"));
        assert!(display.ends_with("末尾"));
        assert_eq!(display.matches("长想法").count(), 12_000);
        assert!(listing.notes[0].annotation.stale);
        assert!(listing.notes[0].chapter_index.is_none());
    }

    #[test]
    fn plain_text_display_escapes_active_content_and_preserves_line_boundaries() {
        assert_eq!(
            plain_text_html("<img src='https://x'>&\"\r\n一\r二\n三"),
            "<p>&lt;img src=&#39;https://x&#39;&gt;&amp;&quot;</p><p>一</p><p>二</p><p>三</p>"
        );
    }

    const THOUGHT_MARKDOWN: &str = "# 想法标题\n\n**重点**、*强调*与 `inline_code`。\n\n> 引用理解\n\n- 第一项\n- 第二项\n\n1. 步骤一\n2. 步骤二\n\n```python\nprint('code_value')\n```\n\n| 字段 | 含义 |\n| --- | --- |\n| source | 证据 |\n\n[链接标签](https://example.invalid/hidden)\n\n![图片说明](https://example.invalid/image)\n\n<img src='https://example.invalid/raw'>\n";

    #[test]
    fn human_and_ai_thoughts_share_safe_markdown_and_keep_original_copy_and_search() {
        use ::markdown::{ParseOptions, mdast::Node, to_mdast};

        let mut listing = NoteListing::default();
        let notes = [AnnotationKind::HumanComment, AnnotationKind::AiComment]
            .into_iter()
            .enumerate()
            .map(|(index, kind)| {
                let mut note = note(index, "书", kind);
                note.annotation.anchor.quote = "**引文保留标点**".into();
                note.annotation.comment = Some(THOUGHT_MARKDOWN.into());
                note
            })
            .collect();
        listing.replace(notes);
        assert_eq!(listing.cards.len(), 2);
        assert_eq!(
            listing.cards[0].comment_markdown,
            listing.cards[1].comment_markdown
        );
        for card in &listing.cards {
            let note = &listing.notes[card.index];
            assert_eq!(note.annotation.comment.as_deref(), Some(THOUGHT_MARKDOWN));
            assert!(copy_text(note).ends_with(THOUGHT_MARKDOWN));
            assert!(card.quote_html.contains("**引文保留标点**"));
            let display = &card.comment_markdown.as_ref().unwrap().markdown;
            let tree = to_mdast(display, &ParseOptions::gfm()).unwrap();
            let mut structures = HashSet::new();
            let mut pending = vec![&tree];
            while let Some(node) = pending.pop() {
                let structure = match node {
                    Node::Heading(_) => Some("heading"),
                    Node::Strong(_) => Some("strong"),
                    Node::Emphasis(_) => Some("emphasis"),
                    Node::InlineCode(_) => Some("inline-code"),
                    Node::List(_) => Some("list"),
                    Node::Blockquote(_) => Some("quote"),
                    Node::Code(_) => Some("code"),
                    Node::Table(_) => Some("table"),
                    Node::Html(_)
                    | Node::Image(_)
                    | Node::ImageReference(_)
                    | Node::Link(_)
                    | Node::LinkReference(_) => panic!("active content: {node:?}"),
                    _ => None,
                };
                if let Some(structure) = structure {
                    structures.insert(structure);
                }
                if let Some(children) = node.children() {
                    pending.extend(children);
                }
            }
            for structure in [
                "heading",
                "strong",
                "emphasis",
                "inline-code",
                "list",
                "quote",
                "code",
                "table",
            ] {
                assert!(
                    structures.contains(structure),
                    "missing {structure}: {display}"
                );
            }
            assert!(display.contains("链接标签"));
            assert!(display.contains("图片说明"));
            assert!(!display.contains("https://example.invalid/hidden"));
            assert!(!display.contains("https://example.invalid/image"));
        }
        // Search still finds the original Markdown, including a destination
        // deliberately omitted from the inert presentation.
        listing.query = "https://example.invalid/hidden".into();
        listing.apply_filter();
        assert_eq!(listing.cards.len(), 2);
    }

    #[test]
    fn table_segments_keep_all_cell_text_inline_markdown_and_neighboring_paragraphs() {
        let long_cell = format!(
            "**重点**、*强调*、`inline_code` {} CELL_END",
            "完整文字 ".repeat(200)
        );
        let source = format!(
            "开始 **说明**\n\n| 字段 | 详情 |\n| --- | --- |\n| source \\| suffix | {long_cell} |\n\n结束说明"
        );
        let display = ThoughtDisplay::new(assistant_display_markdown(&source));
        assert_eq!(display.parts.len(), 3);
        assert!(
            matches!(&display.parts[0], ThoughtPart::Markdown(text) if text.contains("开始 **说明**"))
        );
        let ThoughtPart::Table { rows, nested } = &display.parts[1] else {
            panic!("top-level table must use native cells");
        };
        assert!(!nested);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].len(), 2);
        assert_eq!(rows[0][0].to_string(), "字段");
        assert_eq!(rows[1][0].to_string(), "source \\| suffix");
        assert_eq!(rows[1][1].to_string(), long_cell);
        assert!(
            matches!(&display.parts[2], ThoughtPart::Markdown(text) if text.contains("结束说明"))
        );
        // The normal Markdown renderer never receives this table, so its
        // built-in cell truncation cannot hide the saved tail.
        assert!(display.parts.iter().all(|part| match part {
            ThoughtPart::Markdown(text) => !text.contains("CELL_END"),
            ThoughtPart::Table { .. } => true,
        }));
    }

    #[test]
    fn nested_tables_get_a_complete_native_view_and_preserve_surrounding_markdown() {
        let source = "> 引用说明\n>\n> | 名称 | 内容 |\n> | --- | --- |\n> | 条目 | **粗体** 与 `code` NESTED_END |\n\n后续正文";
        let display = ThoughtDisplay::new(assistant_display_markdown(source));
        let tables = display
            .parts
            .iter()
            .filter_map(|part| match part {
                ThoughtPart::Table { rows, nested } => Some((rows, nested)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(tables.len(), 1);
        assert!(*tables[0].1);
        assert_eq!(
            tables[0].0[1][1].to_string(),
            "**粗体** 与 `code` NESTED_END"
        );
        assert!(
            display.parts.iter().any(
                |part| matches!(part, ThoughtPart::Markdown(text) if text.contains("后续正文"))
            )
        );
    }

    struct TextProbe;

    impl Render for TextProbe {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("notes-text-probe")
                .debug_selector(|| "notes-text-probe".into())
                .w(px(420.))
                .h(px(180.))
                .text_size(px(16.))
                .child(selectable_note_text(
                    "notes-text-selection".into(),
                    plain_text_html("**THOUGHT**\n<img src='https://example.invalid/a'>").into(),
                    px(400.),
                    window,
                    cx,
                ))
        }
    }

    fn redraw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, app| window.draw(app).clear());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn displayed_quote_is_selectable_plain_text_including_markdown_and_html(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let text = cx.new(|_| TextProbe);
            Root::new(text, window, cx)
        });
        redraw(visual);
        let bounds = visual.debug_bounds("notes-text-probe").unwrap();
        let start = bounds.origin + point(px(1.), px(1.));
        let end = bounds.origin + point(px(395.), px(170.));
        visual.simulate_mouse_move(start, None, Modifiers::none());
        visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        redraw(visual);
        visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
        redraw(visual);
        visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
        redraw(visual);
        visual.write_to_clipboard(ClipboardItem::new_string("unhandled-copy".into()));
        #[cfg(target_os = "macos")]
        visual.simulate_keystrokes("cmd-c");
        #[cfg(not(target_os = "macos"))]
        visual.simulate_keystrokes("ctrl-c");
        let copied = visual.read_from_clipboard().unwrap().text().unwrap();
        assert!(copied.contains("**THOUGHT**"), "{copied:?}");
        assert!(
            copied.contains("<img src='https://example.invalid/a'>"),
            "{copied:?}"
        );
    }

    struct ThoughtProbe {
        markdown: Arc<ThoughtDisplay>,
    }

    impl Render for ThoughtProbe {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("notes-thought-probe")
                .debug_selector(|| "notes-thought-probe".into())
                .w(px(480.))
                .h(px(440.))
                .text_size(px(16.))
                .child(render_note_thought(
                    "notes-thought-selection".into(),
                    self.markdown.clone(),
                    px(460.),
                    window,
                    cx,
                ))
        }
    }

    #[gpui::test]
    fn human_and_ai_markdown_display_selection_reads_formatted_text(cx: &mut TestAppContext) {
        const SOURCE: &str = "# TITLE\n\n**BOLD**\n\n- FIRST_ITEM\n- SECOND_ITEM\n\n[LINK_LABEL](https://example.invalid/link)\n\n![IMAGE_LABEL](https://example.invalid/image)\n\n<img src='https://example.invalid/raw'>";
        cx.update(gpui_component::init);
        for kind in [AnnotationKind::HumanComment, AnnotationKind::AiComment] {
            let mut note = note(0, "书", kind);
            note.annotation.comment = Some(SOURCE.into());
            let mut listing = NoteListing::default();
            listing.replace(vec![note]);
            let markdown = listing.cards[0].comment_markdown.clone().unwrap();
            let (_, visual) = cx.add_window_view(|window, cx| {
                let text = cx.new(|_| ThoughtProbe { markdown });
                Root::new(text, window, cx)
            });
            redraw(visual);
            let bounds = visual.debug_bounds("notes-thought-probe").unwrap();
            let start = bounds.origin + point(px(1.), px(1.));
            let end = bounds.origin + point(px(450.), px(430.));
            visual.simulate_mouse_move(start, None, Modifiers::none());
            visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
            redraw(visual);
            visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
            redraw(visual);
            visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
            redraw(visual);
            visual.write_to_clipboard(ClipboardItem::new_string("unhandled-copy".into()));
            #[cfg(target_os = "macos")]
            visual.simulate_keystrokes("cmd-c");
            #[cfg(not(target_os = "macos"))]
            visual.simulate_keystrokes("ctrl-c");
            let copied = visual.read_from_clipboard().unwrap().text().unwrap();
            for visible in [
                "TITLE",
                "BOLD",
                "FIRST_ITEM",
                "SECOND_ITEM",
                "LINK_LABEL",
                "IMAGE_LABEL",
                "<img src='https://example.invalid/raw'>",
            ] {
                assert!(
                    copied.contains(visible),
                    "{kind:?} missing {visible}: {copied:?}"
                );
            }
            assert!(!copied.contains("# TITLE"), "{copied:?}");
            assert!(!copied.contains("**BOLD**"), "{copied:?}");
            assert!(
                !copied.contains("https://example.invalid/link"),
                "{copied:?}"
            );
            assert!(
                !copied.contains("https://example.invalid/image"),
                "{copied:?}"
            );
            assert!(copy_text(&listing.notes[0]).ends_with(SOURCE));
        }
    }

    struct TableProbe {
        display: Arc<ThoughtDisplay>,
    }

    impl Render for TableProbe {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("notes-table-probe")
                .debug_selector(|| "notes-table-probe".into())
                .w(px(400.))
                .h(px(680.))
                .text_size(px(16.))
                .child(render_note_thought(
                    "notes-table-view".into(),
                    self.display.clone(),
                    px(400.),
                    window,
                    cx,
                ))
        }
    }

    #[gpui::test]
    fn native_table_wraps_long_cells_and_scrolls_to_later_columns(cx: &mut TestAppContext) {
        let long_cell = format!(
            "**BOLD** *EMPHASIS* `CODE` {} TABLE_END",
            "Every word remains readable. ".repeat(8)
        );
        let source = format!(
            "| First | Second | Third | Last |\n| --- | --- | --- | --- |\n| {long_cell} | Two | Three | LAST_COLUMN |"
        );
        let display = Arc::new(ThoughtDisplay::new(assistant_display_markdown(&source)));
        cx.update(gpui_component::init);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let table = cx.new(|_| TableProbe { display });
            Root::new(table, window, cx)
        });
        redraw(visual);
        let cell = visual
            .debug_bounds("notes-table-view-part-0-cell-1-0")
            .unwrap();
        assert!(cell.size.height > px(100.), "long cell must wrap: {cell:?}");
        assert!(cell.bottom() < px(680.), "{cell:?}");
        let start = cell.origin + point(px(13.), px(13.));
        let end = point(cell.right() - px(14.), cell.bottom() - px(13.));
        visual.simulate_mouse_move(start, None, Modifiers::none());
        visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        redraw(visual);
        visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
        redraw(visual);
        visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
        redraw(visual);
        visual.write_to_clipboard(ClipboardItem::new_string("unhandled-copy".into()));
        #[cfg(target_os = "macos")]
        visual.simulate_keystrokes("cmd-c");
        #[cfg(not(target_os = "macos"))]
        visual.simulate_keystrokes("ctrl-c");
        let copied = visual.read_from_clipboard().unwrap().text().unwrap();
        for expected in ["BOLD", "EMPHASIS", "CODE", "TABLE_END"] {
            assert!(copied.contains(expected), "lost {expected}: {copied:?}");
        }
        assert!(!copied.contains("**BOLD**"));
        let last_before = visual
            .debug_bounds("notes-table-view-part-0-cell-1-3")
            .unwrap();
        assert!(last_before.left() >= px(400.));
        let position = point(px(200.), cell.top() + px(20.));
        visual.simulate_mouse_move(position, None, Modifiers::none());
        visual.simulate_event(ScrollWheelEvent {
            position,
            delta: ScrollDelta::Pixels(point(px(-1_000.), px(0.))),
            ..Default::default()
        });
        redraw(visual);
        let last_after = visual
            .debug_bounds("notes-table-view-part-0-cell-1-3")
            .unwrap();
        assert!(
            last_after.left() < last_before.left(),
            "{last_before:?} -> {last_after:?}"
        );
        assert!(
            last_after.right() <= px(400.),
            "last column must be reachable: {last_after:?}"
        );
    }
}
