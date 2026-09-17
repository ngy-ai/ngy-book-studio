//! Importing and creating books.

use super::*;

impl LibraryStore {
    pub fn import(&mut self, source_path: &Path) -> Result<ImportOutcome> {
        tracing::info!(target: "ngy_import", path = %source_path.display(), "图书库：开始导入");
        let imported = self
            .registry
            .import_path(source_path, &ImportLimits::default())
            .inspect_err(|error| {
                tracing::error!(
                    target: "ngy_import",
                    path = %source_path.display(),
                    error = %format!("{error:#}"),
                    "图书库：导入解析失败"
                );
            })?;
        if let Some(existing) = self
            .books
            .iter()
            .find(|book| book.id == imported.document.id)
        {
            tracing::info!(
                target: "ngy_import",
                book_id = %existing.id,
                title = %existing.title,
                "图书库：内容哈希已存在，跳过写入"
            );
            return Ok(ImportOutcome::AlreadyExists(existing.clone()));
        }
        let original = imported
            .original_asset()
            .context("导入器没有保留字节一致的原文件")?;
        let source_name = imported_source_name(&imported.document);
        let media_type = original.metadata.media_type.clone();
        let source_bytes = original.bytes.clone();
        tracing::info!(
            target: "ngy_import",
            book_id = %imported.document.id,
            title = %imported.document.title,
            media_type = %media_type,
            source_name = ?source_name,
            units = imported.document.units.len(),
            assets = imported.document.assets.len(),
            "图书库：开始持久化"
        );
        let record = self
            .persist_document(
                imported,
                source_bytes.as_slice(),
                &media_type,
                source_name,
                "original",
                None,
            )
            .inspect_err(|error| {
                tracing::error!(
                    target: "ngy_import",
                    path = %source_path.display(),
                    error = %format!("{error:#}"),
                    "图书库：持久化失败"
                );
            })?;
        tracing::info!(
            target: "ngy_import",
            book_id = %record.id,
            title = %record.title,
            "图书库：导入已提交"
        );
        Ok(self.finish_committed_import(record))
    }

    /// Updates the in-memory projection after an import transaction has
    /// committed. Cover hydration is deliberately best-effort here: failing
    /// to read a derived UI cache must not turn a durable import into an
    /// apparent failure. In particular, callers such as
    /// `AppServices::spawn_library` must still observe `Ok` so they wake the
    /// persisted derivative-index queue.
    pub(crate) fn finish_committed_import(&mut self, record: BookRecord) -> ImportOutcome {
        self.progress_incarnations
            .insert(record.id.clone(), next_progress_incarnation());
        self.books.insert(0, record.clone());
        if let Err(error) = self.reload_cover(&record.id) {
            tracing::warn!(
                book_id = %record.id,
                error = %format!("{error:#}"),
                "图书导入已提交，但封面缓存加载失败；继续以无封面状态完成导入"
            );
        }
        ImportOutcome::Added(record)
    }

    /// Creates an editable book with one HTML chapter. A generated EPUB is
    /// kept as its current view/export projection; it is not treated as an
    /// imported original.
    pub fn create_book(&mut self, title: &str, author: &str) -> Result<BookRecord> {
        let title = title.trim();
        ensure!(!title.is_empty(), "图书名称不能为空");
        let now = now_nanos();
        let book_id = deterministic_id("book", format!("created\0{title}\0{now}").as_bytes());
        let unit_id = deterministic_id("unit", format!("{book_id}\0chapter-1").as_bytes());
        let document = BlockDocument::new(vec![
            Block::heading(
                deterministic_id("block", format!("{unit_id}\0heading").as_bytes()),
                1,
                "第一章",
            ),
            Block::paragraph(
                deterministic_id("block", format!("{unit_id}\0paragraph").as_bytes()),
                "开始写作……",
            ),
        ]);
        let source = crate::markup::serialize_source(&document)?;
        let unit = DocumentUnit::new(
            unit_id.clone(),
            ContentUnitKind::Chapter,
            "第一章",
            source,
            document,
        )
        .with_source_locator(SourceLocator::created());
        let mut book = BookDocument::created(book_id, title);
        if !author.trim().is_empty() {
            book.authors.push(author.trim().to_string());
        }
        book.units.push(unit);
        book.toc.push(TocNode::new(
            deterministic_id("toc", format!("{}\0chapter-1", book.id).as_bytes()),
            "第一章",
            TocTarget::unit(unit_id),
        ));
        book.validate()?;
        let epub = generated_epub(&book)?;
        let imported = ImportedBook {
            document: book,
            assets: Vec::new(),
        };
        let record = self.persist_document(
            imported,
            &epub,
            "application/epub+zip",
            None,
            "created",
            None,
        )?;
        self.progress_incarnations
            .insert(record.id.clone(), next_progress_incarnation());
        self.books.insert(0, record.clone());
        Ok(record)
    }
}
