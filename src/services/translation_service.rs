//! Manual translation, translation display and reader zoom.

use super::*;

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
                        // A manual edit is displayed regardless of which model or
                        // endpoint produced the machine text behind it: the reader
                        // wrote it, and a model change must not hide it.
                        let manual = row.manual_text.as_deref().and_then(|manual| {
                            serde_json::from_str::<crate::ai::translation::StoredTranslation>(
                                manual,
                            )
                            .ok()
                        });
                        if let Some(translation) = manual {
                            return Some(TranslatedBlock {
                                key: row.block_id,
                                source: row.source_text,
                                segments: translation.segments,
                                manual: true,
                            });
                        }
                        if row.model != translation_model {
                            return None;
                        }
                        let translation = serde_json::from_str::<
                            crate::ai::translation::StoredTranslation,
                        >(&row.translated_text)
                        .ok()?;
                        if translation.execution_identity != execution_identity {
                            return None;
                        }
                        Some(TranslatedBlock {
                            key: row.block_id,
                            source: row.source_text,
                            segments: translation.segments,
                            manual: false,
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
}
