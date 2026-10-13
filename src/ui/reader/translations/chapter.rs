//! 「立即翻译本章」：让当前这一章先于整本书翻完。

use super::*;

impl ReaderApp {
    /// 把当前正在读的这一章排到翻译队列最前。
    ///
    /// 整本翻译是一本书一条任务、游标按块顺序走，所以这里交给服务层去收窄推进点：
    /// 正在跑的整本翻译先落盘，这一章翻完再回到原来的位置继续（见
    /// `AppServices::translate_reader_chapter_now`）。窗口这一侧只负责说出「哪一章」——
    /// 内容单元取自当前章节的稳定 locator，与服务层其它入口同一份来源。
    ///
    /// 完成时不重推译文：这一章此刻还没翻完，库里还是旧的；后台轮询
    /// （`start_translation_refresh`）会在新译文落库时重新推送这一章。
    pub(crate) fn translate_current_chapter_now(&mut self, cx: &mut Context<Self>) {
        if self.closing || self.translations.chapter_pending {
            return;
        }
        let Some(locator) = self.progress_locators.get(self.current_spine) else {
            return;
        };
        let unit_id = locator.unit_id.clone();
        let book_id = self.book_id.clone();
        let services = Arc::clone(&self.services);
        self.translations.chapter_pending = true;
        self.notice = None;
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = services
                .translate_reader_chapter_now(book_id, unit_id)
                .await;
            let _ = view.update(cx, |this, cx| this.finish_translate_chapter_now(result, cx));
        })
        .detach();
    }

    pub(crate) fn finish_translate_chapter_now(
        &mut self,
        result: anyhow::Result<()>,
        cx: &mut Context<Self>,
    ) {
        self.translations.chapter_pending = false;
        if self.closing {
            return;
        }
        match result {
            Ok(()) => {
                self.notice = Some(Notice {
                    text: "已把本章排到翻译队列最前，翻完这一章会回到原来的位置继续整本；\
                           新译文落库后这一章自动更新。"
                        .to_string(),
                    error: false,
                });
            }
            Err(error) => {
                self.notice = Some(Notice {
                    text: format!("无法立即翻译本章：{error:#}"),
                    error: true,
                });
            }
        }
        cx.notify();
    }
}
