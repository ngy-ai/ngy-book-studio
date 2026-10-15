//! Manual translation, translation display and reader zoom.

use super::*;
use crate::ai::translation::{TranslationSegment, normalize_source_text};

/// 等一个正在运行的整本翻译落盘的上限与轮询间隔。
///
/// 「立即翻译本章」只在它发布「已暂停」之后才规划新的推进点：运行中的执行器拥有游标，
/// 直接改写会被它随后的写入覆盖。暂停请求在执行的请求之间被读到，所以上限给得比一轮
/// 控制检查（`CONTROL_POLL_INTERVAL`）宽得多。
const TRANSLATION_CHAPTER_PAUSE_TIMEOUT: Duration = Duration::from_secs(15);
const TRANSLATION_CHAPTER_PAUSE_POLL: Duration = Duration::from_millis(100);

impl AppServices {
    /// Returns the current, revision-matched translations for one content unit
    /// in the configured default display language. Returns an empty vector when
    /// translation is disabled, the book already uses the target language, the
    /// unit belongs to another book, or no up-to-date translations exist yet.
    pub async fn translation_blocks_for_unit(
        &self,
        book_id: String,
        content_unit_id: String,
    ) -> Result<Vec<TranslatedBlock>> {
        let settings = self.provider_settings()?;
        let Some(target_language) = settings.default_language.clone() else {
            return Ok(Vec::new());
        };
        let translation_model = settings.translation_model.clone();
        let execution_identity = translation_execution_identity(&settings)?;
        let db_path = self.db_path.clone();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let conn = db::open_conn(&db_path)?;
                let Some(book) = db::books::get(&conn, &book_id)? else {
                    return Ok(Vec::new());
                };
                if book
                    .language
                    .as_deref()
                    .is_some_and(|source| language_is_target(Some(source), &target_language))
                {
                    return Ok(Vec::new());
                }
                let Some(unit) = db::content_units::get(&conn, &content_unit_id)? else {
                    return Ok(Vec::new());
                };
                if unit.book_id != book_id {
                    return Ok(Vec::new());
                }
                let rows =
                    db::translations::list_for_unit(&conn, &content_unit_id, &target_language)?;
                Ok(rows
                    .into_iter()
                    .filter(|row| {
                        row.document_revision == book.revision && row.unit_revision == unit.revision
                    })
                    .filter_map(|row| {
                        let (segments, manual) =
                            displayed_translation(&row, &translation_model, &execution_identity)?;
                        Some(TranslatedBlock {
                            key: row.block_id,
                            source: row.source_text,
                            segments,
                            manual,
                        })
                    })
                    .collect())
            })
            .await
            .context("译文查询线程异常退出")?
    }

    /// Saves the reader's own translation of one block, or clears it with `None`
    /// so the block falls back to the machine text.
    ///
    /// The reading window is untrusted input: the host reloads the row itself and
    /// requires the submitted segments to line up with the stored machine
    /// segments, so a request can only replace the translated text of a block
    /// that is genuinely current. Sources are always taken from the stored row,
    /// never from the request.
    pub async fn set_manual_translation(
        &self,
        book_id: String,
        content_unit_id: String,
        block_id: String,
        segments: Option<Vec<crate::ai::translation::TranslationSegment>>,
    ) -> Result<()> {
        ensure!(!book_id.trim().is_empty(), "图书 ID 不能为空");
        ensure!(!content_unit_id.trim().is_empty(), "内容单元 ID 不能为空");
        ensure!(!block_id.trim().is_empty(), "文本块 ID 不能为空");
        if let Some(segments) = &segments {
            validate_manual_translation_segments(segments)?;
        }
        let settings = self.provider_settings()?;
        let Some(target_language) = settings.default_language.clone() else {
            anyhow::bail!("尚未配置默认译文语言，无法保存手工译文");
        };
        let db_path = self.db_path.clone();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let conn = db::open_conn(&db_path)?;
                let Some(book) = db::books::get(&conn, &book_id)? else {
                    anyhow::bail!("图书不存在，无法保存手工译文");
                };
                let Some(unit) = db::content_units::get(&conn, &content_unit_id)? else {
                    anyhow::bail!("章节不存在，无法保存手工译文");
                };
                ensure!(unit.book_id == book_id, "该章节不属于当前图书");
                let rows =
                    db::translations::list_for_unit(&conn, &content_unit_id, &target_language)?;
                let Some(row) = rows.into_iter().find(|row| row.block_id == block_id) else {
                    anyhow::bail!("该文本块没有可修改的译文");
                };
                ensure!(
                    row.document_revision == book.revision && row.unit_revision == unit.revision,
                    "该文本块的译文已过期，请重新打开本章后再修改"
                );
                let manual_text = match segments {
                    None => None,
                    Some(segments) => {
                        let machine = serde_json::from_str::<
                            crate::ai::translation::StoredTranslation,
                        >(&row.translated_text)
                        .context("该文本块的机器译文不可用，无法保存手工译文")?;
                        let segments = manual_translation_segments(&machine, &segments)?;
                        Some(
                            serde_json::to_string(&crate::ai::translation::StoredTranslation {
                                execution_identity: machine.execution_identity,
                                segments,
                            })
                            .context("无法序列化手工译文")?,
                        )
                    }
                };
                ensure!(
                    manual_text
                        .as_ref()
                        .is_none_or(|text| text.len() <= MAX_MANUAL_TRANSLATION_BYTES),
                    "手工译文过长"
                );
                let updated = db::translations::set_manual_text(
                    &conn,
                    &book_id,
                    &content_unit_id,
                    &block_id,
                    &target_language,
                    book.revision,
                    unit.revision,
                    manual_text.as_deref(),
                    unix_timestamp()?,
                )?;
                ensure!(updated, "该文本块的译文已过期，请重新打开本章后再修改");
                Ok(())
            })
            .await
            .context("手工译文保存线程异常退出")?
    }

    /// Re-runs the model translation of one text block the reading window shows.
    ///
    /// 阅读端只说得出块标识（`TranslatedBlock::key`），序号由任务自己的文本块列表判定
    /// （见 `IndexingCoordinator::retranslate_translation_block`）；重跑的是当前显示
    /// 语言对应的那个任务 —— 同一本书可以有多语言任务，各自独立。
    ///
    /// 只影响机器译文：读者手工改过的那一块仍然显示读者的文字（手工列不被机器发布
    /// 覆盖），想看到新的机器文本要用「恢复机器译文」。
    pub async fn retranslate_reader_translation_block(
        &self,
        book_id: String,
        content_unit_id: String,
        block_key: String,
    ) -> Result<()> {
        ensure!(!book_id.trim().is_empty(), "图书 ID 不能为空");
        ensure!(!content_unit_id.trim().is_empty(), "内容单元 ID 不能为空");
        ensure!(!block_key.trim().is_empty(), "文本块 ID 不能为空");
        let Some(target_language) = self.provider_settings()?.default_language.clone() else {
            anyhow::bail!("尚未配置默认译文语言，无法重新翻译");
        };
        let job_id = self.translation_job_id(&book_id, &target_language).await?;
        let indexing = Arc::clone(&self.indexing);
        let retried = self
            .runtime
            .spawn(async move {
                indexing
                    .retranslate_translation_block(&job_id, &content_unit_id, &block_key)
                    .await
            })
            .await
            .context("翻译重试线程异常退出")??;
        // 拒绝的三种情形都说清楚：这本书没有翻译任务、重跑的那一块在正文里找不到、
        // 或者任务此刻正在运行（重试路径不打断执行器，得先把任务暂停）。
        ensure!(
            retried,
            "无法重新翻译该文本块：这本书没有可重试的翻译任务（尚未配置翻译，或任务正在运行，\
             请先暂停任务）"
        );
        Ok(())
    }

    /// 「立即翻译本章」：把这一章排到翻译队列最前，正在跑的整本翻译先落盘再让位。
    ///
    /// 整本翻译是一本书一条任务、游标按块顺序推进，所以「只翻这一章」是把任务的推进点借给
    /// 这一章的块（`IndexingCoordinator::translate_translation_chapter`）：这一章翻完之后
    /// walk 回到借出之前的位置继续整本，中间没翻的章节不会被跳过。同一次规划还把任务写到
    /// 待执行队列的最前面（游标里的 `priority`），其它书的翻译、向量与视觉任务因此排在它后面。
    ///
    /// 运行中的执行器拥有游标，所以这里先请求暂停并等它发布「已暂停」再规划；等不到就明确
    /// 报错，绝不与执行器抢游标。
    pub async fn translate_reader_chapter_now(
        &self,
        book_id: String,
        content_unit_id: String,
    ) -> Result<()> {
        ensure!(!book_id.trim().is_empty(), "图书 ID 不能为空");
        ensure!(!content_unit_id.trim().is_empty(), "内容单元 ID 不能为空");
        let Some(target_language) = self.provider_settings()?.default_language.clone() else {
            anyhow::bail!("尚未配置默认译文语言，无法立即翻译本章");
        };
        let job_id = self.translation_job_id(&book_id, &target_language).await?;
        self.pause_running_translation(&job_id).await?;
        let indexing = Arc::clone(&self.indexing);
        let planned = self
            .runtime
            .spawn(async move {
                indexing
                    .translate_translation_chapter(&job_id, &content_unit_id)
                    .await
            })
            .await
            .context("翻译任务规划线程异常退出")??;
        ensure!(
            planned,
            "无法立即翻译本章：这本书没有可排队的翻译任务（尚未配置翻译，或任务已被重排）"
        );
        Ok(())
    }

    /// 这本书当前来源、这个目标语言的翻译任务 ID。
    ///
    /// 任务 ID 由**来源**与语言派生（不是图书）：重新导入会换来源，也就是换一条任务。
    async fn translation_job_id(&self, book_id: &str, target_language: &str) -> Result<String> {
        let db_path = self.db_path.clone();
        let book_id = book_id.to_string();
        let target_language = target_language.to_string();
        self.runtime
            .handle()
            .spawn_blocking(move || -> anyhow::Result<String> {
                let conn = db::open_conn(&db_path)?;
                let book = db::books::get(&conn, &book_id)?.context("图书不存在")?;
                let source = db::book_sources::get_revision(&conn, &book_id, book.revision)?
                    .context("图书来源不存在")?;
                Ok(format!("translation:{}:{target_language}", source.id))
            })
            .await
            .context("翻译任务查询线程异常退出")?
    }

    /// 请求暂停一个正在运行的整本翻译，并等它把「已暂停」落盘。
    ///
    /// 已经是终态或已暂停的任务什么都不做：规划路径本来就能改写它们的游标。
    ///
    /// 这一等必须能在没有 Tokio 上下文的调用者上跑：阅读窗口是从 GPUI 的前台执行器调用
    /// 这个入口的（`docs/agent/code-map.md` 的约定，`services` 测试里的
    /// `block_on_without_tokio` 复现的正是这种环境）。现场 2026-10-14：这里直接用
    /// `tokio::time::sleep`，在 GPUI 回调里 panic（`there is no reactor running`），而 GPUI
    /// 回调不可 unwind，整个进程随之退出 —— 等待因此派到应用 I/O runtime 自己的阻塞池上。
    async fn pause_running_translation(&self, job_id: &str) -> Result<()> {
        if self.indexing.job(job_id).await?.map(|job| job.status)
            != Some(IndexingJobStatus::Running)
        {
            return Ok(());
        }
        self.control_background_job(job_id.to_string(), BackgroundJobAction::Pause)
            .await?;
        let deadline = std::time::Instant::now() + TRANSLATION_CHAPTER_PAUSE_TIMEOUT;
        loop {
            if self.indexing.job(job_id).await?.map(|job| job.status)
                != Some(IndexingJobStatus::Running)
            {
                return Ok(());
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "整本翻译还在处理当前文本块，暂时无法把本章排到最前（可以稍后再点一次，\
                 或先到「后台任务」窗口暂停这本书的翻译）"
            );
            self.wait_before_pause_poll().await?;
        }
    }

    /// 两次轮询之间的一次等待，落在应用 I/O runtime 的阻塞池上。
    ///
    /// 不能用 `tokio::time::sleep`：它要求调用方自己处在 Tokio 上下文里（没有反应堆时
    /// panic），而这个入口的调用者是 GPUI 的前台执行器。`spawn_blocking` 只需要运行时
    /// 句柄，等待期间也不占 GPUI 的线程。
    async fn wait_before_pause_poll(&self) -> Result<()> {
        self.runtime
            .handle()
            .spawn_blocking(|| std::thread::sleep(TRANSLATION_CHAPTER_PAUSE_POLL))
            .await
            .context("等待整本翻译落盘的线程异常退出")?;
        Ok(())
    }

    /// One book's own translation display choice, written by the reading window.
    /// `None` means the book still follows the global preference, which is also
    /// what an unreadable row falls back to: a corrupt display preference must
    /// never stop a chapter from opening.
    pub async fn translation_display_override(
        &self,
        book_id: String,
    ) -> Result<Option<TranslationDisplayMode>> {
        ensure!(!book_id.trim().is_empty(), "图书 ID 不能为空");
        let key = db::settings::translation_display_book_key(&book_id);
        let db_path = self.db_path.clone();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let conn = db::open_conn(&db_path)?;
                Ok(db::settings::get(&conn, &key)?
                    .and_then(|row| {
                        serde_json::from_str::<PersistedBookTranslationDisplay>(&row.value_json)
                            .ok()
                    })
                    .map(|persisted| persisted.display_mode))
            })
            .await
            .context("译文显示方式查询线程异常退出")?
    }

    /// Stores one book's display choice, or clears it with `None` so the book
    /// follows the global preference again. Book deletion removes the row with
    /// the book; see `db::transactions::delete_document`.
    pub async fn set_translation_display_override(
        &self,
        book_id: String,
        mode: Option<TranslationDisplayMode>,
    ) -> Result<()> {
        ensure!(!book_id.trim().is_empty(), "图书 ID 不能为空");
        let key = db::settings::translation_display_book_key(&book_id);
        let db_path = self.db_path.clone();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let mut conn = db::open_conn(&db_path)?;
                let tx = conn.transaction().context("无法开始保存译文显示方式")?;
                match mode {
                    Some(mode) => {
                        let row = db::settings::Setting {
                            key: key.clone(),
                            value_json: serde_json::to_string(&PersistedBookTranslationDisplay {
                                display_mode: mode,
                            })
                            .context("无法序列化译文显示方式")?,
                            updated_at: unix_timestamp()?,
                        };
                        ensure!(
                            db::settings::upsert(&tx, &row)? == 1,
                            "译文显示方式未能保存"
                        );
                    }
                    None => {
                        db::settings::delete(&tx, &key)?;
                    }
                }
                tx.commit().context("无法提交译文显示方式")?;
                Ok(())
            })
            .await
            .context("译文显示方式保存线程异常退出")?
    }

    /// One book's remembered page size for one reading surface, written by the
    /// reading window. `None` means the surface still uses its own default,
    /// which is also what an unreadable row falls back to: a corrupt page size
    /// must never stop a book from opening. The value comes back unclamped
    /// because every surface measures its page in its own unit and clamps what
    /// it reads against its own range.
    pub async fn reader_zoom(
        &self,
        book_id: String,
        surface: ReaderZoomSurface,
    ) -> Result<Option<u32>> {
        ensure!(!book_id.trim().is_empty(), "图书 ID 不能为空");
        let key = surface.key(&book_id);
        let db_path = self.db_path.clone();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let conn = db::open_conn(&db_path)?;
                Ok(db::settings::get(&conn, &key)?
                    .and_then(|row| {
                        serde_json::from_str::<PersistedReaderZoom>(&row.value_json).ok()
                    })
                    .map(|persisted| persisted.value))
            })
            .await
            .context("阅读页面大小查询线程异常退出")?
    }

    /// Stores one surface's page size for one book. Book deletion removes the
    /// row with the book; see `db::transactions::delete_document`.
    pub async fn set_reader_zoom(
        &self,
        book_id: String,
        surface: ReaderZoomSurface,
        value: u32,
    ) -> Result<()> {
        ensure!(!book_id.trim().is_empty(), "图书 ID 不能为空");
        let key = surface.key(&book_id);
        let db_path = self.db_path.clone();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let mut conn = db::open_conn(&db_path)?;
                let tx = conn.transaction().context("无法开始保存阅读页面大小")?;
                let row = db::settings::Setting {
                    key,
                    value_json: serde_json::to_string(&PersistedReaderZoom { value })
                        .context("无法序列化阅读页面大小")?,
                    updated_at: unix_timestamp()?,
                };
                ensure!(
                    db::settings::upsert(&tx, &row)? == 1,
                    "阅读页面大小未能保存"
                );
                tx.commit().context("无法提交阅读页面大小")?;
                Ok(())
            })
            .await
            .context("阅读页面大小保存线程异常退出")?
    }

    /// Translations of a reading window's table-of-contents labels, aligned with
    /// the request: index `i` answers `labels[i]`, and `None` keeps the label the
    /// book itself carries.
    ///
    /// A label is matched by its exact, whitespace-normalized text, which is the
    /// only key the reading window and the translation task share: the label of a
    /// chapter entry is normally the chapter's own heading, so the译文 of that
    /// block is the label's译文. The chapter the entry opens is preferred, and a
    /// book-wide match is the fallback for entries whose label does not repeat a
    /// heading of its chapter (navigation labels are written by hand, they do not
    /// have to). Past [`MAX_READER_TOC_LABELS`] distinct label texts the remaining
    /// entries simply keep the book's own label instead of turning this into an
    /// unbounded query.
    pub async fn translated_toc_labels(
        &self,
        book_id: String,
        labels: Vec<ReaderTocLabel>,
    ) -> Result<Vec<Option<String>>> {
        if labels.is_empty() {
            return Ok(Vec::new());
        }
        let settings = self.provider_settings()?;
        let Some(target_language) = settings.default_language.clone() else {
            return Ok(vec![None; labels.len()]);
        };
        let translation_model = settings.translation_model.clone();
        let execution_identity = translation_execution_identity(&settings)?;
        let db_path = self.db_path.clone();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let mut translated = vec![None; labels.len()];
                let conn = db::open_conn(&db_path)?;
                let Some(book) = db::books::get(&conn, &book_id)? else {
                    return Ok(translated);
                };
                if book
                    .language
                    .as_deref()
                    .is_some_and(|source| language_is_target(Some(source), &target_language))
                {
                    return Ok(translated);
                }
                // 标签文本先归一化再查：书内导航的空白与正文里的空白写法不同，
                // 但同一段文字必须落到同一把键上。
                let texts = labels
                    .iter()
                    .map(|label| normalize_source_text(&label.text))
                    .filter(|text| !text.is_empty())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .take(MAX_READER_TOC_LABELS)
                    .collect::<Vec<_>>();
                if texts.is_empty() {
                    return Ok(translated);
                }
                let rows = db::translations::list_with_source_texts(
                    &conn,
                    &book_id,
                    &target_language,
                    book.revision,
                    &texts,
                )?;
                // 目录标签是短文本，同一段文字在一本书里可能出现在多处（同名小节、
                // 导航页自身、正文里的同名标题）。读者自己改过的那一份永远优先，其余
                // 取文档顺序里最靠前的一行；按单元再存一份，指向该章的条目因此优先
                // 用这一章自己的译文。
                let mut resolved = Vec::with_capacity(rows.len());
                for row in &rows {
                    let Some((segments, manual)) = displayed_translation(
                        &row.translation,
                        &translation_model,
                        &execution_identity,
                    ) else {
                        continue;
                    };
                    let Some(text) = translation_display_text(&segments) else {
                        continue;
                    };
                    resolved.push((row, text, manual));
                }
                let mut by_unit = BTreeMap::new();
                let mut by_text = BTreeMap::new();
                for manual in [true, false] {
                    for (row, text, is_manual) in &resolved {
                        if *is_manual != manual {
                            continue;
                        }
                        by_unit
                            .entry((
                                row.content_unit_id.clone(),
                                row.translation.source_text.clone(),
                            ))
                            .or_insert_with(|| text.clone());
                        by_text
                            .entry(row.translation.source_text.clone())
                            .or_insert_with(|| text.clone());
                    }
                }
                for (index, label) in labels.iter().enumerate() {
                    let key = normalize_source_text(&label.text);
                    if key.is_empty() {
                        continue;
                    }
                    let translated_label = label
                        .content_unit_id
                        .as_ref()
                        .and_then(|unit| by_unit.get(&(unit.clone(), key.clone())))
                        .or_else(|| by_text.get(&key));
                    if let Some(text) = translated_label {
                        translated[index] = Some(text.clone());
                    }
                }
                Ok(translated)
            })
            .await
            .context("目录译文查询线程异常退出")?
    }

    /// The translation engine a share import stamps shared machine译文 with.
    ///
    /// The reader hides machine译文 unless their model and execution identity
    /// match the active settings, so a package would otherwise only be visible
    /// to a friend running the exact same endpoint and model. Importing a
    /// package is an explicit adoption, so the receiving side re-stamps the
    /// shared rows with its own engine.
    ///
    /// `None` when no translation model is configured: the shared rows then keep
    /// the sender's identity and stay hidden until the receiving settings line
    /// up, which is honest rather than inventing an engine that produced them.
    pub fn share_translation_stamp(&self) -> Option<crate::share::ShareTranslationStamp> {
        let settings = self.provider_settings().ok()?;
        if settings.translation_model.trim().is_empty() {
            return None;
        }
        let execution_identity = translation_execution_identity(&settings).ok()?;
        Some(crate::share::ShareTranslationStamp {
            model: settings.translation_model.clone(),
            execution_identity,
            target_language: settings.default_language.clone(),
        })
    }
}

/// Upper bound on the table-of-contents labels one reading window asks译文 for.
/// A navigation document is written by hand and stays far below this in
/// practice; a crafted one only loses译文 labels past the bound instead of
/// turning the label lookup into an unbounded query.
pub(crate) const MAX_READER_TOC_LABELS: usize = 4096;

/// The译文 one row displays right now: the reader's own edit when there is one,
/// otherwise the machine text that matches the active engine.
///
/// Both the chapter payload and the table-of-contents labels go through this, so
/// a label can never display text the chapter itself would hide (and the
/// "已手工修改" state survives a model or endpoint change in both places).
fn displayed_translation(
    row: &db::translations::Translation,
    translation_model: &str,
    execution_identity: &str,
) -> Option<(Vec<TranslationSegment>, bool)> {
    if let Some(translation) = row.manual_text.as_deref().and_then(|manual| {
        serde_json::from_str::<crate::ai::translation::StoredTranslation>(manual).ok()
    }) {
        return Some((translation.segments, true));
    }
    if row.model != translation_model {
        return None;
    }
    let translation =
        serde_json::from_str::<crate::ai::translation::StoredTranslation>(&row.translated_text)
            .ok()?;
    if translation.execution_identity != execution_identity {
        return None;
    }
    Some((translation.segments, false))
}

/// One block's译文 as a single line of text.
///
/// A block is translated leaf by leaf, so the译文 of a heading with inline markup
/// arrives as several segments. The chapter renders each segment in place of its
/// own leaf, which means the leaf's boundary whitespace belongs to the displayed
/// text; recreating that here keeps two segments from running together
/// (`函数` + `f(x)` must not become `函数f(x)` when the source had a space).
/// Whitespace runs collapse back to single spaces and the ends are trimmed,
/// because a label is one line.
fn translation_display_text(segments: &[TranslationSegment]) -> Option<String> {
    let mut text = String::new();
    for segment in segments {
        let leading = segment.source.len() - segment.source.trim_start().len();
        let trailing = segment.source.len() - segment.source.trim_end().len();
        text.push_str(&segment.source[..leading]);
        text.push_str(segment.translated.trim());
        text.push_str(&segment.source[segment.source.len() - trailing..]);
    }
    let text = normalize_source_text(&text);
    (!text.is_empty()).then_some(text)
}
