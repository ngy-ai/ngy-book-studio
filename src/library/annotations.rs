//! Annotation validation, creation and deletion.

use super::*;

impl LibraryStore {
    /// Checks against the exact EPUB chapter served to the reading WebView,
    /// including original EPUB markup and headings added by normalized export.
    /// Call on a background worker because resource parsing performs I/O.
    pub fn validate_annotation_anchor(
        &self,
        book_id: &str,
        content_unit_id: &str,
        document_revision: u64,
        unit_revision: u64,
        anchor: &TextAnchor,
    ) -> Result<()> {
        ensure!(anchor.start < anchor.end, "请选择非空文本后添加笔记");
        ensure!(
            anchor.quote.len() <= crate::annotations::MAX_ANNOTATION_QUOTE_BYTES,
            "所选文本过长，请缩小笔记范围"
        );
        ensure!(
            !crate::annotations::compact_text(&anchor.quote).is_empty(),
            "请选择非空文本后添加笔记"
        );
        let document = self.document(book_id)?;
        ensure!(
            document.revision.get() == document_revision,
            "图书已更新，请重新打开章节后添加笔记"
        );
        let (ordinal, unit) = document
            .units
            .iter()
            .enumerate()
            .find(|(_, unit)| unit.id == content_unit_id)
            .context("笔记章节不属于当前图书")?;
        ensure!(
            unit.revision.get() == unit_revision,
            "章节已更新，请重新打开后添加笔记"
        );
        let opened = crate::reader::OpenedBook::open_bytes(self.reader_epub_bytes(book_id)?)?;
        ensure!(
            opened.spine.len() == document.units.len(),
            "阅读章节与图书结构不一致，请重新打开图书"
        );
        let href = &opened.spine[ordinal].href;
        let response = crate::reader::load_resource(&opened.epub, href)?;
        let html = std::str::from_utf8(&response.bytes).context("无法解码笔记所在章节")?;
        let text = crate::annotations::reader_body_text(html);
        crate::annotations::validate_anchor(&text, anchor)
    }

    /// PDF notes are anchored to the pinned PDF.js text layer of one page.
    ///
    /// No host extractor reproduces that projection byte-for-byte, so the
    /// quote cannot be compared against the canonical page text the way EPUB
    /// chapters are: doing so would reject ordinary selections on any page
    /// whose PDF.js layout differs from the importer's. Offsets stay stable
    /// because the imported original and the bundled PDF.js build are both
    /// immutable, so verify every ownership, scope and revision rule plus the
    /// anchor's own bounds instead.
    pub fn validate_pdf_annotation_anchor(
        &self,
        book_id: &str,
        content_unit_id: &str,
        document_revision: u64,
        unit_revision: u64,
        anchor: &TextAnchor,
    ) -> Result<()> {
        ensure!(anchor.start < anchor.end, "请选择非空文本后添加笔记");
        ensure!(
            anchor.quote.len() <= crate::annotations::MAX_ANNOTATION_QUOTE_BYTES,
            "所选文本过长，请缩小笔记范围"
        );
        let compacted = crate::annotations::compact_text(&anchor.quote);
        ensure!(!compacted.is_empty(), "请选择非空文本后添加笔记");
        // The page range and its quote are two views of one selection: the
        // reader derives both from the same compacted text layer, so their
        // lengths must agree even though the page text itself is not readable
        // here. A range that cannot describe this quote is never stored.
        ensure!(
            u64::from(anchor.end - anchor.start) == compacted.encode_utf16().count() as u64,
            "所选文本与页面位置不一致，请重新选择后添加笔记"
        );
        let document = self.document(book_id)?;
        ensure!(
            document.revision.get() == document_revision,
            "图书已更新，请重新打开页面后添加笔记"
        );
        let unit = document
            .units
            .iter()
            .find(|unit| unit.id == content_unit_id)
            .context("笔记页面不属于当前图书")?;
        ensure!(unit.kind == ContentUnitKind::Page, "笔记位置不是 PDF 页面");
        ensure!(
            unit.revision.get() == unit_revision,
            "页面已更新，请重新打开后添加笔记"
        );
        Ok(())
    }

    /// Persists a PDF page note. Shares the single notes table, the exclusive
    /// mark replacement and every revision rule with EPUB notes; only the
    /// canonical-text comparison is replaced by
    /// [`Self::validate_pdf_annotation_anchor`].
    pub fn create_pdf_annotation(
        &mut self,
        book_id: &str,
        draft: &AnnotationDraft,
    ) -> Result<Annotation> {
        crate::annotations::validate_draft(draft)?;
        self.validate_pdf_annotation_anchor(
            book_id,
            &draft.content_unit_id,
            draft.document_revision,
            draft.unit_revision,
            &draft.anchor,
        )?;
        let now = now_secs();
        let note = Annotation {
            id: deterministic_id("annotation", format!("{book_id}\0{}", now_nanos())),
            book_id: book_id.to_string(),
            content_unit_id: draft.content_unit_id.clone(),
            document_revision: draft.document_revision,
            unit_revision: draft.unit_revision,
            anchor: draft.anchor.clone(),
            kind: draft.kind,
            comment: draft.comment.clone(),
            created_at: now,
            updated_at: now,
            stale: false,
        };
        let mut conn = db::open_conn(&self.db_path)?;
        db::transactions::insert_annotation(&mut conn, &note)
    }

    /// Removes only the mark on this exact PDF selection. Thoughts and other
    /// occurrences of the same quote are retained; an unmarked range succeeds.
    pub fn delete_pdf_annotation_marks(
        &mut self,
        book_id: &str,
        content_unit_id: &str,
        document_revision: u64,
        unit_revision: u64,
        anchor: &TextAnchor,
    ) -> Result<()> {
        self.validate_pdf_annotation_anchor(
            book_id,
            content_unit_id,
            document_revision,
            unit_revision,
            anchor,
        )?;
        let mut conn = db::open_conn(&self.db_path)?;
        db::transactions::delete_annotation_marks(
            &mut conn,
            book_id,
            content_unit_id,
            document_revision,
            unit_revision,
            anchor,
        )
    }

    /// All marks and both kinds of thought share one persisted notes table.
    /// A mark replaces the style of the same exact selection, retaining its
    /// identity; repeating the existing style leaves the note unchanged.
    pub fn create_annotation(
        &mut self,
        book_id: &str,
        draft: &AnnotationDraft,
    ) -> Result<Annotation> {
        crate::annotations::validate_draft(draft)?;
        self.validate_annotation_anchor(
            book_id,
            &draft.content_unit_id,
            draft.document_revision,
            draft.unit_revision,
            &draft.anchor,
        )?;
        let now = now_secs();
        let note = Annotation {
            id: deterministic_id("annotation", format!("{book_id}\0{}", now_nanos())),
            book_id: book_id.to_string(),
            content_unit_id: draft.content_unit_id.clone(),
            document_revision: draft.document_revision,
            unit_revision: draft.unit_revision,
            anchor: draft.anchor.clone(),
            kind: draft.kind,
            comment: draft.comment.clone(),
            created_at: now,
            updated_at: now,
            stale: false,
        };
        let mut conn = db::open_conn(&self.db_path)?;
        db::transactions::insert_annotation(&mut conn, &note)
    }

    pub fn list_annotations(
        &self,
        book_id: &str,
        content_unit_id: Option<&str>,
    ) -> Result<Vec<Annotation>> {
        let conn = db::open_conn(&self.db_path)?;
        let book = db::books::get(&conn, book_id)?.context("图书不存在")?;
        let source = db::book_sources::get_revision(&conn, book_id, book.revision)?
            .context("当前图书来源不存在")?;
        let units = db::content_units::list_for_source(&conn, &source.id)?
            .into_iter()
            .map(|unit| (unit.id, unit.revision))
            .collect::<HashMap<_, _>>();
        let mut notes = db::annotations::list(&conn, book_id, content_unit_id)?;
        for note in &mut notes {
            note.stale = note.document_revision != book.revision
                || units.get(&note.content_unit_id) != Some(&note.unit_revision);
        }
        Ok(notes)
    }

    /// Reads notes for one book, or the entire library, newest update first.
    /// Queries current catalog metadata directly without loading chapter bodies
    /// or relying on this store clone's cached book projection.
    pub fn annotation_overview(&self, book_id: Option<&str>) -> Result<Vec<AnnotationOverview>> {
        let conn = db::open_conn(&self.db_path)?;
        if let Some(book_id) = book_id {
            db::books::get(&conn, book_id)?.context("图书不存在")?;
        }
        db::annotations::overview(&conn, book_id)
    }

    /// Only the body of a human thought is editable. Its author kind, quote,
    /// and original location cannot be changed through this operation.
    pub fn update_human_comment(
        &mut self,
        book_id: &str,
        id: &str,
        comment: &str,
    ) -> Result<Annotation> {
        crate::annotations::validate_comment(
            comment,
            crate::annotations::AnnotationKind::HumanComment,
        )?;
        let conn = db::open_conn(&self.db_path)?;
        let mut existing = self
            .list_annotations(book_id, None)?
            .into_iter()
            .find(|note| note.id == id)
            .context("笔记不存在或不属于当前图书")?;
        ensure!(
            existing.kind == crate::annotations::AnnotationKind::HumanComment,
            "只能编辑人工想法，AI 想法保留原始来源"
        );
        let updated_at = now_secs().max(existing.updated_at);
        ensure!(
            db::annotations::update_human_comment(&conn, book_id, id, comment, updated_at)? == 1,
            "笔记已被删除，无法保存想法"
        );
        existing.comment = Some(comment.to_string());
        existing.updated_at = updated_at;
        Ok(existing)
    }

    pub fn delete_annotation(&mut self, book_id: &str, id: &str) -> Result<()> {
        let conn = db::open_conn(&self.db_path)?;
        ensure!(
            db::annotations::delete(&conn, book_id, id)? == 1,
            "笔记不存在或不属于当前图书"
        );
        Ok(())
    }

    /// Removes only the mark on this exact selection. Thoughts and other
    /// occurrences of the same quote are retained; an unmarked range succeeds.
    pub fn delete_annotation_marks(
        &mut self,
        book_id: &str,
        content_unit_id: &str,
        document_revision: u64,
        unit_revision: u64,
        anchor: &TextAnchor,
    ) -> Result<()> {
        self.validate_annotation_anchor(
            book_id,
            content_unit_id,
            document_revision,
            unit_revision,
            anchor,
        )?;
        let mut conn = db::open_conn(&self.db_path)?;
        db::transactions::delete_annotation_marks(
            &mut conn,
            book_id,
            content_unit_id,
            document_revision,
            unit_revision,
            anchor,
        )
    }
}
