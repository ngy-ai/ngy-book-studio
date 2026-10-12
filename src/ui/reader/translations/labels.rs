//! 目录标签跟随显示方式：显示译文时用条目所指章节标题的译文。
//!
//! 章节条目的标签来自书自己的导航文档，写的是书的语言。窗口显示译文时，条目改用
//! 它指向的那一章里、它所命名的那段文字的译文：章节标题和段落一样是普通文本块，
//! 所以标签与标题不会互相矛盾。没有译文的条目——这一章还没译到，或者标签根本没对
//! 应某个块——继续显示书里自己的写法，目录因此不会出现空白或猜测。
//!
//! 这里不落库：窗口按目录缓存一份答案，任务推进或读者手工改过译文后再取一次。
use super::*;
use ngy_book_studio::reader::TocItem;

/// 与 `toc` 同序、一条一个请求。
///
/// 内容单元取自阅读进度定位表：条目的 `spine_index` 是它要打开的章，单元则是译文
/// 落库时用的键。只起分组作用、`href` 没落到任何一章的条目没有单元，它的标签按全书
/// 匹配。
pub(crate) fn toc_label_requests(
    toc: &[TocItem],
    locators: &[DocumentLocator],
) -> Vec<ReaderTocLabel> {
    toc.iter()
        .map(|item| ReaderTocLabel {
            content_unit_id: item
                .spine_index
                .and_then(|index| locators.get(index))
                .map(|locator| locator.unit_id.clone()),
            text: item.label.clone(),
        })
        .collect()
}

/// 一个条目显示用的标签：显示译文时用译文，选「原文」或还没有译文时用书里自己的写法。
pub(crate) fn toc_label(
    original: &str,
    translated: Option<&str>,
    mode: TranslationDisplayMode,
) -> String {
    if mode == TranslationDisplayMode::OriginalOnly {
        return original.to_string();
    }
    translated.unwrap_or(original).to_string()
}

impl ReaderApp {
    /// 请服务取出目录每个条目的译文。答案与 `self.book.toc` 一一对应，所以迟到的回答
    /// 只会被丢掉，目录本身不会在它手上变形状。
    pub(crate) fn refresh_navigation_labels(&mut self, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        let labels = toc_label_requests(&self.book.toc, &self.progress_locators);
        if labels.is_empty() {
            self.translations.navigation_labels = Vec::new();
            return;
        }
        self.translations.navigation_generation =
            self.translations.navigation_generation.wrapping_add(1);
        let generation = self.translations.navigation_generation;
        let book_id = self.book_id.clone();
        let services = Arc::clone(&self.services);
        cx.spawn(async move |view, cx| {
            let result = services.translated_toc_labels(book_id, labels).await;
            let _ = view.update(cx, |this, cx| {
                if this.closing || this.translations.navigation_generation != generation {
                    return;
                }
                let labels = match result {
                    Ok(labels) => labels,
                    Err(error) => {
                        // 目录译文只是显示层：取不到就继续显示书里的原文，
                        // 和读不到本书显示方式时一样只留一条警告，不打断阅读。
                        tracing::warn!(%error, "无法加载目录译文");
                        return;
                    }
                };
                if labels == this.translations.navigation_labels {
                    return;
                }
                this.translations.navigation_labels = labels;
                cx.notify();
            });
        })
        .detach();
    }

    /// 阅读窗口为某个目录条目显示的标签。
    pub(crate) fn navigation_label(&self, index: usize) -> String {
        let Some(item) = self.book.toc.get(index) else {
            return String::new();
        };
        toc_label(
            &item.label,
            self.translations
                .navigation_labels
                .get(index)
                .and_then(Option::as_deref),
            self.translations.effective,
        )
    }
}
