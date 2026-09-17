//! Publishing an edited document: the transaction that replaces units,
//! assets and the table of contents together.

use super::*;

impl LibraryStore {
    /// Saves a validated canonical document as a normalized EPUB revision.
    /// A stale caller is rejected by the revision check; imported originals
    /// remain referenced as immutable assets.
    pub fn apply_document(&mut self, document: BookDocument) -> Result<BookRecord> {
        self.apply_document_with_assets(document, HashMap::new())
    }

    /// Publishes a canonical revision together with newly inserted immutable
    /// assets. Existing asset bytes are resolved only after a book-ownership
    /// check; supplied bytes must exactly match their document metadata.
    pub fn apply_document_with_assets(
        &mut self,
        document: BookDocument,
        new_asset_bytes: HashMap<String, Arc<Vec<u8>>>,
    ) -> Result<BookRecord> {
        self.publish_document_with_assets(document, new_asset_bytes)
            .map(|published| published.record)
    }

    /// Publishes a canonical revision and reports the revision every chapter
    /// received, so a caller that keeps an in-memory projection of the saved
    /// document does not have to guess: a chapter whose body did not change
    /// keeps its own revision and therefore its notes and whole-book译文.
    pub fn publish_document_with_assets(
        &mut self,
        mut document: BookDocument,
        new_asset_bytes: HashMap<String, Arc<Vec<u8>>>,
    ) -> Result<PublishedDocument> {
        let position = self
            .books
            .iter()
            .position(|book| book.id == document.id)
            .context("图书不存在")?;
        let current = self.book_record(&document.id)?;
        ensure!(
            document.revision.get() == current.revision,
            "图书已被其它窗口修改，请重新打开后再保存"
        );
        document.validate()?;
        for (asset_id, bytes) in &new_asset_bytes {
            let metadata = document
                .find_asset(asset_id)
                .with_context(|| format!("新资源不在文档模型中：{asset_id}"))?;
            ensure!(metadata.byte_len == bytes.len() as u64, "新资源长度不匹配");
            ensure!(
                metadata.content_hash == blake3::hash(bytes).to_hex().to_string(),
                "新资源内容哈希不匹配"
            );
        }
        let resolver = |asset_id: &str| match new_asset_bytes.get(asset_id) {
            Some(bytes) => Ok(bytes.as_ref().clone()),
            None => self.asset_bytes(&document.id, asset_id),
        };
        let new_bytes =
            BuiltinDocumentExporter.export_bytes(&document, ExportFormat::Epub, &resolver)?;
        let published = self.published_unit_content(&document.id, current.revision)?;
        document.revision = Revision::new(current.revision + 1);
        for unit in &mut document.units {
            // Only a chapter whose body really changed takes the new revision.
            // Every other chapter keeps its own revision, so its notes and its
            // whole-book translation stay valid instead of being translated
            // again only because another chapter was edited.
            unit.revision = match published.get(&unit.id) {
                Some((revision, hash)) if hash == &unit.content_hash() => Revision::new(*revision),
                _ => document.revision,
            };
        }
        let assets = document
            .assets
            .iter()
            .cloned()
            .map(|metadata| {
                let bytes = resolver(&metadata.id)?;
                Ok(ImportedAsset {
                    metadata,
                    bytes: Arc::new(bytes),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let book_id = document.id.clone();
        let title = document.title.clone();
        let unit_revisions = document
            .units
            .iter()
            .map(|unit| unit.revision)
            .collect::<Vec<_>>();
        let imported = ImportedBook { document, assets };
        let updated = self.persist_document(
            imported,
            &new_bytes,
            "application/epub+zip",
            Some(format!("{}.epub", safe_file_stem(&title))),
            "normalized",
            Some(&current),
        )?;
        self.books[position] = updated.clone();
        if let Err(error) = self.reload_cover(&book_id) {
            tracing::warn!(
                %book_id,
                revision = updated.revision,
                error = %format!("{error:#}"),
                "图书保存已提交，但封面缓存加载失败；已保留持久修订，重新加载图书时将重试封面"
            );
        }
        Ok(PublishedDocument {
            record: updated,
            unit_revisions,
        })
    }

    /// Content hash and own revision of every chapter of one published
    /// revision. `apply_document_with_assets` uses this to keep a chapter's
    /// revision — and therefore its notes and译文 — when an edit did not change
    /// that chapter's body. A chapter whose stored structure cannot be read is
    /// reported as changed so nothing is ever reused by accident.
    pub(crate) fn published_unit_content(
        &self,
        book_id: &str,
        revision: u64,
    ) -> Result<HashMap<String, (u64, String)>> {
        let conn = db::open_conn(&self.db_path)?;
        let Some(source) = db::book_sources::get_revision(&conn, book_id, revision)? else {
            return Ok(HashMap::new());
        };
        Ok(db::content_units::list_for_source(&conn, &source.id)?
            .into_iter()
            .map(|unit| {
                let hash = serde_json::from_str::<BlockDocument>(&unit.block_json)
                    .map(|document| document.content_hash())
                    .unwrap_or_default();
                (unit.id, (unit.revision, hash))
            })
            .collect())
    }

    /// Parses and cleans a source edit before atomically publishing the next
    /// canonical revision. Invalid HTML never reaches SQLite.
    pub fn update_content_unit_source(
        &mut self,
        book_id: &str,
        unit_id: &str,
        source: &str,
    ) -> Result<BookRecord> {
        let document = self.document(book_id)?;
        let mut editor = crate::editing::DocumentEditor::new(document)?;
        editor.update_unit_source(unit_id, source)?;
        self.apply_document(editor.into_document())
    }

    pub(crate) fn persist_document(
        &self,
        imported: ImportedBook,
        current_source_bytes: &[u8],
        source_media_type: &str,
        source_name: Option<String>,
        source_kind: &str,
        previous: Option<&BookRecord>,
    ) -> Result<BookRecord> {
        imported.validate(&ImportLimits::default())?;
        // This method is called on an AppServices blocking worker (or during
        // startup/tests), never from a GPUI entity callback. Keep every object
        // put and the reference-publishing SQLite transaction under one gate
        // so a concurrent collector cannot remove a reused content hash in the
        // gap between those two operations.
        let publication_guard = self.io.block_on(self.blob_publication.acquire());
        let source_key = self
            .io
            .block_on(self.blob_store.put(current_source_bytes))?;
        let mut asset_keys = HashMap::new();
        for asset in &imported.assets {
            let key = self
                .io
                .block_on(self.blob_store.put(asset.bytes.as_slice()))?;
            ensure!(
                key == BlobKey::from_bytes(asset.bytes.as_slice()),
                "对象存储返回了错误的内容地址"
            );
            asset_keys.insert(asset.metadata.id.clone(), key);
        }
        let mut conn = db::open_conn(&self.db_path)?;
        let previous_progress = previous
            .map(|record| db::progress::get(&conn, &record.id))
            .transpose()?
            .flatten();
        let graph = build_persisted_graph(
            imported,
            source_key,
            current_source_bytes.len() as u64,
            source_media_type,
            source_name,
            source_kind,
            asset_keys,
            previous,
            previous_progress.as_ref(),
        )?;
        // `graph.book` is the exact catalog row validated and published by
        // the transaction below. Do not issue a fallible SELECT after commit:
        // a transient read error at that point would misreport an already
        // durable import/save as failed and leave the UI projection stale.
        let committed_record = graph.book.clone();
        let auto_run_background_jobs = self.auto_run_background_jobs.load(Ordering::Acquire);
        let unreferenced = if previous.is_some() {
            db::transactions::install_document_revision(
                &mut conn,
                &graph.borrow(),
                auto_run_background_jobs,
            )?
        } else {
            db::transactions::insert_document(
                &mut conn,
                &graph.borrow(),
                auto_run_background_jobs,
            )?;
            Vec::new()
        };
        drop(conn);
        drop(publication_guard);
        self.schedule_blob_deletions(unreferenced);
        Ok(committed_record)
    }

    pub(crate) fn read_blob(&self, object_key: &str) -> Result<Vec<u8>> {
        let key = BlobKey::parse(object_key)?;
        self.io.block_on(self.blob_store.get(&key))
    }
}
