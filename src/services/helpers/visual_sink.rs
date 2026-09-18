//! The library-backed visual document source and the local page sink.

use super::*;

#[derive(Clone)]
pub(crate) struct LibraryVisualDocumentSource {
    pub(crate) library: Arc<Mutex<LibraryStore>>,
}

impl VisualDocumentSource for LibraryVisualDocumentSource {
    fn load_document(
        &self,
        book_id: String,
        revision: crate::document::Revision,
    ) -> PreviewFuture<'_, crate::document::BookDocument> {
        let library = Arc::clone(&self.library);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let document = library
                    .lock()
                    .map_err(|_| anyhow::anyhow!("library service lock is poisoned"))?
                    .document(&book_id)?;
                ensure!(
                    document.revision == revision,
                    "visual request revision is no longer current"
                );
                Ok(document)
            })
            .await
            .context("visual document loader stopped")?
        })
    }

    fn load_asset(
        &self,
        book_id: String,
        asset_id: String,
    ) -> PreviewFuture<'_, VisualAssetPayload> {
        let library = Arc::clone(&self.library);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let library = library
                    .lock()
                    .map_err(|_| anyhow::anyhow!("library service lock is poisoned"))?;
                let (media_type, expected_len) = library.asset_metadata(&book_id, &asset_id)?;
                ensure!(
                    media_type.starts_with("image/"),
                    "视觉任务只能读取图书所属图片"
                );
                let bytes = library.asset_bytes(&book_id, &asset_id)?;
                ensure!(
                    bytes.len() as u64 == expected_len,
                    "视觉图片对象长度与数据库元数据不一致"
                );
                Ok(VisualAssetPayload { media_type, bytes })
            })
            .await
            .context("visual asset loader stopped")?
        })
    }

    fn load_source(
        &self,
        book_id: String,
        source_id: String,
        revision: crate::document::Revision,
    ) -> PreviewFuture<'_, VisualSourcePayload> {
        let library = Arc::clone(&self.library);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let library = library
                    .lock()
                    .map_err(|_| anyhow::anyhow!("library service lock is poisoned"))?;
                let (format, source_kind, media_type, bytes) =
                    library.visual_source_bytes(&book_id, &source_id, revision)?;
                Ok(VisualSourcePayload {
                    format,
                    source_kind,
                    media_type,
                    bytes,
                })
            })
            .await
            .context("visual source loader stopped")?
        })
    }
}

#[derive(Clone)]
pub(crate) struct LocalVisualPageSink {
    pub(crate) db_path: PathBuf,
    pub(crate) blobs: Arc<LocalBlobStore>,
    pub(crate) blob_publication: BlobPublicationLock,
    pub(crate) indexing: Arc<IndexingCoordinator>,
}

impl VisualPageSink for LocalVisualPageSink {
    fn reset_staging(&self, spec: crate::preview::VisualJobSpec) -> PreviewFuture<'_, ()> {
        let db_path = self.db_path.clone();
        let blobs = Arc::clone(&self.blobs);
        let blob_publication = self.blob_publication.clone();
        Box::pin(async move {
            let publication_guard = blob_publication.acquire().await;
            let reset_db_path = db_path.clone();
            let unreferenced = tokio::task::spawn_blocking(move || {
                db::transactions::clear_visual_page_staging(
                    &mut db::open_conn(&reset_db_path)?,
                    &spec,
                )
            })
            .await
            .context("visual page staging reset worker stopped")??;
            drop(publication_guard);
            reclaim_unreferenced_blobs(
                &db_path,
                &blobs,
                &blob_publication,
                unreferenced,
                "视觉页面断点对象",
            )
            .await;
            Ok(())
        })
    }

    fn checkpoint_page(
        &self,
        spec: crate::preview::VisualJobSpec,
        page: RenderedVisualPage,
        completed_pages: usize,
    ) -> PreviewFuture<'_, ()> {
        let db_path = self.db_path.clone();
        let blobs = Arc::clone(&self.blobs);
        let blob_publication = self.blob_publication.clone();
        Box::pin(async move {
            let publication_guard = blob_publication.acquire().await;
            let now = unix_timestamp()?;
            let key = blobs.put(&page.bytes).await?;
            ensure!(
                key == crate::storage::BlobKey::from_bytes(&page.bytes),
                "object store returned a non-content-addressed key"
            );
            let blob_row = db::blobs::BlobRecord {
                object_key: key.to_string(),
                media_type: page.media_type.clone(),
                byte_len: page.bytes.len() as u64,
                hash: blake3::hash(&page.bytes).to_hex().to_string(),
                created_at: now,
            };
            let page_row = visual_page_row(&spec, page, key.to_string(), now)?;
            let commit_db_path = db_path.clone();
            tokio::task::spawn_blocking(move || {
                db::transactions::checkpoint_visual_page(
                    &mut db::open_conn(&commit_db_path)?,
                    &spec,
                    &blob_row,
                    &page_row,
                    completed_pages,
                    now,
                )
            })
            .await
            .context("visual page checkpoint worker stopped")??;
            drop(publication_guard);
            Ok(())
        })
    }

    fn commit_pages(
        &self,
        spec: crate::preview::VisualJobSpec,
        total_pages: usize,
    ) -> PreviewFuture<'_, ()> {
        let db_path = self.db_path.clone();
        let blobs = Arc::clone(&self.blobs);
        let blob_publication = self.blob_publication.clone();
        let indexing = Arc::clone(&self.indexing);
        Box::pin(async move {
            let publication_guard = blob_publication.acquire().await;
            let source_id = spec.source_id.clone();
            let commit_db_path = db_path.clone();
            let now = unix_timestamp()?;
            let unreferenced = tokio::task::spawn_blocking(move || {
                db::transactions::publish_staged_visual_pages(
                    &mut db::open_conn(&commit_db_path)?,
                    &spec,
                    total_pages,
                    now,
                )
            })
            .await
            .context("visual page publication worker stopped")??;
            drop(publication_guard);
            reclaim_unreferenced_blobs(
                &db_path,
                &blobs,
                &blob_publication,
                unreferenced,
                "已替换视觉页面对象",
            )
            .await;
            if let Err(error) = indexing.visual_pages_ready(&source_id).await {
                // Final pages and the Succeeded job state were already
                // committed atomically. Vision jobs are durable and will be
                // discovered by the background scanner, so a wake-up failure
                // must not rewrite the completed render as Failed.
                tracing::warn!(source_id, %error, "视觉页面已发布，但视觉索引即时唤醒失败");
            }
            Ok(())
        })
    }
}
