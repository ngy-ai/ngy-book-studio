//! Covers, assets and the media blocks that reference them.

use super::*;

impl LibraryStore {
    pub fn cover_bytes(&self, book_id: &str) -> Result<Option<Arc<Vec<u8>>>> {
        if let Some(bytes) = self.cover_cache.get(book_id) {
            return Ok(Some(bytes.clone()));
        }
        let record = self.book_record(book_id)?;
        record
            .cover_object_key
            .as_deref()
            .map(|key| self.read_blob(key).map(Arc::new))
            .transpose()
    }

    /// Returns only the startup/import hydrated cache and never performs I/O.
    /// This is safe to call from a GPUI render callback.
    pub fn cover_bytes_cached(&self, book_id: &str) -> Option<Arc<Vec<u8>>> {
        self.cover_cache.get(book_id).cloned()
    }

    /// Resolves an asset only after proving that it belongs to the requested
    /// book. This is the same authorization boundary used by exports and the
    /// media protocol.
    pub fn asset_bytes(&self, book_id: &str, asset_id: &str) -> Result<Vec<u8>> {
        let conn = db::open_conn(&self.db_path)?;
        let asset = db::assets::get(&conn, asset_id)?.context("资源不存在")?;
        ensure!(asset.book_id == book_id, "资源不属于指定图书");
        drop(conn);
        self.read_blob(&asset.object_key)
    }

    pub fn asset_range(
        &self,
        book_id: &str,
        asset_id: &str,
        range: std::ops::Range<u64>,
    ) -> Result<Vec<u8>> {
        let conn = db::open_conn(&self.db_path)?;
        let asset = db::assets::get(&conn, asset_id)?.context("资源不存在")?;
        ensure!(asset.book_id == book_id, "资源不属于指定图书");
        drop(conn);
        let key = BlobKey::parse(&asset.object_key)?;
        self.io.block_on(self.blob_store.get_range(&key, range))
    }

    pub fn asset_metadata(&self, book_id: &str, asset_id: &str) -> Result<(String, u64)> {
        let conn = db::open_conn(&self.db_path)?;
        let asset = db::assets::get(&conn, asset_id)?.context("资源不存在")?;
        ensure!(asset.book_id == book_id, "资源不属于指定图书");
        Ok((asset.media_type, asset.byte_len))
    }

    /// Loads one exact persisted source revision for a native visual renderer.
    /// The source ID is opaque outside the storage layer and is checked along
    /// with book ownership and revision before any bytes are returned.
    pub(crate) fn visual_source_bytes(
        &self,
        book_id: &str,
        source_id: &str,
        revision: Revision,
    ) -> Result<(String, String, String, Vec<u8>)> {
        let conn = db::open_conn(&self.db_path)?;
        let source = db::book_sources::get(&conn, source_id)?.context("图书来源不存在")?;
        ensure!(source.book_id == book_id, "图书来源不属于指定图书");
        ensure!(
            source.revision == revision.get(),
            "图书来源 revision 与视觉任务不一致"
        );
        let blob = db::blobs::get(&conn, &source.object_key)?.context("图书来源对象不存在")?;
        let format = source.format;
        let source_kind = source.source_kind;
        let media_type = blob.media_type;
        let expected_len = blob.byte_len;
        drop(conn);
        let bytes = self.read_blob(&source.object_key)?;
        ensure!(
            bytes.len() as u64 == expected_len,
            "图书来源对象长度与数据库元数据不一致"
        );
        Ok((format, source_kind, media_type, bytes))
    }

    pub fn insert_media_block(
        &mut self,
        book_id: &str,
        unit_id: &str,
        position: usize,
        draft: MediaDraft,
    ) -> Result<BookRecord> {
        let mut document = self.document(book_id)?;
        let asset = DocumentAsset::from_bytes(
            book_id,
            draft.kind.role(),
            draft.media_type.clone(),
            draft.original_file_name.clone(),
            draft.bytes.as_slice(),
        );
        let asset_id = asset.id.clone();
        merge_document_asset(&mut document, asset)?;
        let unit = document
            .units
            .iter_mut()
            .find(|unit| unit.id == unit_id)
            .context("内容单元不存在")?;
        ensure!(position <= unit.document.blocks.len(), "媒体插入位置越界");
        let block_id = unique_block_id(unit, &asset_id, position);
        let caption = draft
            .description
            .as_deref()
            .map(|value| vec![crate::document::Inline::text(value)])
            .unwrap_or_default();
        let block = media_block(&draft, block_id, asset_id.clone(), caption);
        unit.document.blocks.insert(position, block);
        unit.source = crate::markup::serialize_source(&unit.document)?;
        document.validate()?;
        self.apply_document_with_assets(document, HashMap::from([(asset_id, draft.bytes.clone())]))
    }

    pub fn replace_media_block(
        &mut self,
        book_id: &str,
        unit_id: &str,
        block_id: &str,
        draft: MediaDraft,
    ) -> Result<BookRecord> {
        let mut document = self.document(book_id)?;
        let asset = DocumentAsset::from_bytes(
            book_id,
            draft.kind.role(),
            draft.media_type.clone(),
            draft.original_file_name.clone(),
            draft.bytes.as_slice(),
        );
        let asset_id = asset.id.clone();
        merge_document_asset(&mut document, asset)?;
        let unit = document
            .units
            .iter_mut()
            .find(|unit| unit.id == unit_id)
            .context("内容单元不存在")?;
        let replaced =
            replace_media_in_blocks(&mut unit.document.blocks, block_id, &draft, &asset_id);
        ensure!(replaced, "媒体块不存在");
        unit.source = crate::markup::serialize_source(&unit.document)?;
        prune_unreferenced_assets(&mut document);
        document.validate()?;
        self.apply_document_with_assets(document, HashMap::from([(asset_id, draft.bytes.clone())]))
    }

    pub fn delete_media_block(
        &mut self,
        book_id: &str,
        unit_id: &str,
        block_id: &str,
    ) -> Result<BookRecord> {
        let mut document = self.document(book_id)?;
        let unit = document
            .units
            .iter_mut()
            .find(|unit| unit.id == unit_id)
            .context("内容单元不存在")?;
        ensure!(
            delete_media_from_blocks(&mut unit.document.blocks, block_id),
            "媒体块不存在"
        );
        unit.source = crate::markup::serialize_source(&unit.document)?;
        prune_unreferenced_assets(&mut document);
        document.validate()?;
        self.apply_document(document)
    }

    pub fn set_cover(&mut self, book_id: &str, cover: CoverDraft) -> Result<BookRecord> {
        let mut document = self.document(book_id)?;
        let mut asset = DocumentAsset::from_bytes(
            book_id,
            AssetRole::Cover,
            cover.mime.clone(),
            None,
            cover.bytes.as_slice(),
        );
        asset.add_role(AssetRole::ContentImage);
        let asset_id = asset.id.clone();
        merge_document_asset(&mut document, asset)?;
        document.cover_asset_id = Some(asset_id.clone());
        prune_unreferenced_assets(&mut document);
        document.validate()?;
        self.apply_document_with_assets(document, HashMap::from([(asset_id, cover.bytes.clone())]))
    }

    pub fn clear_cover(&mut self, book_id: &str) -> Result<BookRecord> {
        let mut document = self.document(book_id)?;
        document.cover_asset_id = None;
        prune_unreferenced_assets(&mut document);
        document.validate()?;
        self.apply_document(document)
    }

    pub(crate) fn reload_cover(&mut self, book_id: &str) -> Result<()> {
        self.cover_cache.remove(book_id);
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_cover_cache_hydration) {
            bail!("测试注入的封面缓存加载失败");
        }
        let Some(record) = self.books.iter().find(|book| book.id == book_id) else {
            return Ok(());
        };
        if let Some(key) = record.cover_object_key.as_deref() {
            self.cover_cache
                .insert(book_id.to_string(), Arc::new(self.read_blob(key)?));
        }
        Ok(())
    }
}
pub(crate) fn validate_cover_dimensions(width: u32, height: u32) -> Result<()> {
    ensure!(width > 0 && height > 0, "封面图片尺寸无效");
    ensure!(
        width <= MAX_COVER_DIMENSION
            && height <= MAX_COVER_DIMENSION
            && u64::from(width) * u64::from(height) <= MAX_COVER_PIXELS,
        "封面图片尺寸过大"
    );
    Ok(())
}

pub(crate) fn replacement_cover_extension(mime: &str) -> Option<&'static str> {
    match mime.split(';').next()?.trim().to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}
