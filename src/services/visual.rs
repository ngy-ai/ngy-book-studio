//! Visual pages: publication, reading and the Office enhancement gate.

use super::*;

impl AppServices {
    /// Replaces the structural renderer coordinator with an explicitly
    /// supplied renderer set (for example a PDF.js or Office-enhanced worker).
    /// Existing in-flight jobs must be allowed to finish or be cancelled first.
    pub fn replace_visual_renderers(
        &self,
        source: Arc<dyn VisualDocumentSource>,
        sink: Arc<dyn VisualPageSink>,
        renderers: Vec<Arc<dyn VisualRenderer>>,
    ) -> Result<Arc<VisualJobCoordinator>> {
        let descriptors = renderers
            .iter()
            .map(|renderer| renderer.descriptor())
            .collect::<Vec<_>>();
        let coordinator = Arc::new(VisualJobCoordinator::new(
            self.runtime.handle(),
            Arc::new(SqliteVisualJobStore::new(&self.db_path)),
            source,
            sink,
            renderers,
        )?);
        let mut current = self
            .visual_jobs
            .write()
            .map_err(|_| anyhow::anyhow!("visual job service lock is poisoned"))?;
        if let Some(existing) = current.as_ref() {
            ensure!(
                Arc::strong_count(existing) == 1,
                "visual coordinator is in use; cancel or release its jobs before replacing it"
            );
        }
        *current = Some(Arc::clone(&coordinator));
        *self
            .visual_renderer_descriptors
            .write()
            .map_err(|_| anyhow::anyhow!("visual renderer descriptor lock is poisoned"))? =
            descriptors;
        Ok(coordinator)
    }
}

impl AppServices {
    /// Returns the durable per-book opt-in. Absence is deliberately equivalent
    /// to disabled so importing an Office document never starts COM silently.
    #[cfg(target_os = "windows")]
    pub async fn office_enhancement_enabled(&self, book_id: String) -> Result<bool> {
        ensure!(!book_id.trim().is_empty(), "图书 ID 不能为空");
        let db_path = self.db_path.clone();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                db::office_enhancements::is_enabled(&db::open_conn(&db_path)?, &book_id)
            })
            .await
            .context("Office 增强预览设置查询线程异常退出")?
    }

    /// Enables or disables Office COM enhancement for the current source,
    /// replacing only visual-derived pages/chunks/jobs. Canonical content,
    /// full-text chunks and the byte-exact original remain untouched.
    #[cfg(target_os = "windows")]
    pub async fn set_office_enhancement_enabled(
        &self,
        book_id: String,
        enabled: bool,
    ) -> Result<String> {
        ensure!(!book_id.trim().is_empty(), "图书 ID 不能为空");
        // Serialize the setting and source lookup with canonical document
        // mutations. Otherwise an edit could publish a normalized source
        // between preflight and the derived-job replacement.
        let _mutation_turn = self.library_mutations.reserve().enter().await?;
        let db_path = self.db_path.clone();
        let lookup_book_id = book_id.clone();
        let source_id = self
            .runtime
            .handle()
            .spawn_blocking(move || {
                let conn = db::open_conn(&db_path)?;
                let book = db::books::get(&conn, &lookup_book_id)?.context("图书不存在")?;
                let source = db::book_sources::get_revision(&conn, &lookup_book_id, book.revision)?
                    .context("当前图书来源不存在")?;
                if enabled {
                    ensure!(
                        source.source_kind == "original"
                            && matches!(source.format.as_str(), "doc" | "docx" | "pptx" | "xlsx"),
                        "只有尚未规范化编辑的 Office 原件可以启用增强预览"
                    );
                }
                Ok(source.id)
            })
            .await
            .context("Office 增强预览来源查询线程异常退出")??;

        let coordinator = self.visual_jobs().context("视觉任务协调器不可用")?;
        let job_id = format!("visual-render:{source_id}");
        stop_visual_job_before_replacement(&coordinator, &job_id).await?;
        stop_indexing_jobs_before_visual_replacement(&self.indexing, &book_id, &source_id).await?;

        let descriptors = self
            .visual_renderer_descriptors
            .read()
            .map_err(|_| anyhow::anyhow!("visual renderer descriptor lock is poisoned"))?
            .clone();
        let profile = RenderProfile::default();
        let db_path = self.db_path.clone();
        let publication_guard = self.blob_publication.acquire().await;
        let (committed_source_id, reconciliation, committed_spec) = self
            .runtime
            .handle()
            .spawn_blocking(move || {
                let mut conn = db::open_conn(&db_path)?;
                let (source_id, reconciliation) = db::transactions::set_office_enhancement(
                    &mut conn,
                    &book_id,
                    enabled,
                    &descriptors,
                    &profile,
                    unix_timestamp()?,
                )?;
                // The mutation turn prevents another source/settings replacement;
                // the background renderer may only advance this committed job.
                // Freeze its full identity before object reclamation yields.
                let job_id = format!("visual-render:{source_id}");
                let job = db::index_jobs::get(&conn, &job_id)?
                    .context("Office 增强视觉任务提交后不存在")?;
                let (spec, _) = crate::preview::decode_persisted_visual_job(&job.cursor_json)?;
                ensure!(
                    spec.id == job_id && spec.book_id == book_id && spec.source_id == source_id,
                    "Office 增强视觉任务与本次提交的来源不一致"
                );
                Ok((source_id, reconciliation, spec))
            })
            .await
            .context("Office 增强预览设置线程异常退出")??;
        ensure!(
            committed_source_id == source_id,
            "图书来源在 Office 增强预览设置期间发生变化"
        );
        drop(publication_guard);
        reclaim_unreferenced_blobs(
            &self.db_path,
            &self.blobs,
            &self.blob_publication,
            reconciliation.unreferenced_blobs,
            "Office 增强替换的视觉页面对象",
        )
        .await;
        coordinator
            .schedule_committed(&committed_spec)
            .await
            .context("无法确认 Office 增强视觉任务已交给后台处理")?;
        self.indexing.wake();
        Ok(job_id)
    }

    /// Loads the complete, successfully published visual page set for the
    /// book's current source revision.
    ///
    /// The database graph is validated while the object publication gate is
    /// held, then every object is rechecked against its stored length, BLAKE3
    /// digest and image media type before bytes leave the service layer.
    pub async fn load_published_visual_pages(
        &self,
        book_id: String,
        kind: VisualPageSourceKind,
    ) -> Result<Vec<PublishedVisualPage>> {
        ensure!(!book_id.trim().is_empty(), "图书 ID 不能为空");
        // Canonical edits and source-setting operations use this same
        // turnstile. Holding the turn until object reads finish ensures the
        // result still belongs to the current source when it is returned.
        let _mutation_turn = self.library_mutations.reserve().enter().await?;
        let publication_guard = self.blob_publication.acquire().await;
        let db_path = self.db_path.clone();
        let rows = self
            .runtime
            .handle()
            .spawn_blocking(move || load_published_visual_page_rows(&db_path, &book_id, kind))
            .await
            .context("视觉页面查询线程异常退出")??;

        let mut pages = Vec::with_capacity(rows.len());
        let mut total_bytes = 0_u64;
        for (page_index, row) in rows.into_iter().enumerate() {
            let key = BlobKey::parse(&row.object_key).context("视觉页面对象键无效")?;
            let bytes = self.blobs.get(&key).await.context("无法读取视觉页面对象")?;
            ensure!(
                bytes.len() as u64 == row.byte_len,
                "视觉页面对象长度与数据库元数据不一致"
            );
            total_bytes = total_bytes
                .checked_add(row.byte_len)
                .context("视觉页面总大小溢出")?;
            ensure!(
                total_bytes <= MAX_LOADED_VISUAL_PAGE_BYTES,
                "视觉页面总大小超过支持上限"
            );
            let digest = blake3::hash(&bytes).to_hex().to_string();
            ensure!(
                digest == row.hash && BlobKey::from_bytes(&bytes) == key,
                "视觉页面对象摘要与数据库元数据不一致"
            );
            let expected_format = visual_page_image_format(&row.media_type)?;
            ensure!(
                image::guess_format(&bytes).context("无法识别视觉页面图片格式")? == expected_format,
                "视觉页面图片格式与媒体类型不一致"
            );
            pages.push(PublishedVisualPage {
                file_name: visual_page_file_name(page_index, &row.media_type)?,
                media_type: row.media_type,
                bytes,
                content_unit_id: row.content_unit_id,
                locator: row.locator,
                width: row.width,
                height: row.height,
            });
        }
        drop(publication_guard);
        Ok(pages)
    }

    /// Loads the Office-enhanced page set for the book's current source.
    #[cfg(target_os = "windows")]
    pub async fn load_office_enhanced_pages(
        &self,
        book_id: String,
    ) -> Result<Vec<PublishedVisualPage>> {
        self.load_published_visual_pages(book_id, VisualPageSourceKind::OfficeEnhanced)
            .await
    }

    /// Ensures the current source's `visual_render` job is scheduled to run and
    /// returns its id.
    ///
    /// Reading a DjVu book needs its page images, and DjVu rasterization is
    /// local, deterministic safe Rust, so the reader may start it directly
    /// (unlike the Office COM enhancement, which stays behind an explicit
    /// per-book opt-in). A running or already-succeeded job is left alone.
    pub async fn ensure_visual_render_job(&self, book_id: String) -> Result<String> {
        ensure!(!book_id.trim().is_empty(), "图书 ID 不能为空");
        let db_path = self.db_path.clone();
        let lookup_book_id = book_id.clone();
        let (job_id, status) = self
            .runtime
            .handle()
            .spawn_blocking(move || {
                let conn = db::open_conn(&db_path)?;
                let book = db::books::get(&conn, &lookup_book_id)?.context("图书不存在")?;
                let source = db::book_sources::get_revision(&conn, &lookup_book_id, book.revision)?
                    .context("当前图书来源不存在")?;
                let job_id = format!("visual-render:{}", source.id);
                let job = db::index_jobs::get(&conn, &job_id)?
                    .with_context(|| format!("当前来源没有视觉渲染任务：{job_id}"))?;
                ensure!(
                    job.kind == "visual_render"
                        && job.book_id == lookup_book_id
                        && job.source_id.as_deref() == Some(source.id.as_str()),
                    "视觉渲染任务不属于当前图书来源"
                );
                Ok((job_id, job.status))
            })
            .await
            .context("视觉渲染任务查询线程异常退出")??;

        use db::index_jobs::IndexJobStatus;
        match status {
            IndexJobStatus::Succeeded => {}
            IndexJobStatus::Queued | IndexJobStatus::Running | IndexJobStatus::Paused => {
                // Resume keeps the durable page prefix: a job paused from the
                // background-task window must not restart from page 0. Calling
                // resume on an already-running job is harmless.
                let coordinator = self.visual_jobs().context("视觉任务协调器不可用")?;
                coordinator
                    .resume(&job_id)
                    .await
                    .context("无法恢复视觉渲染任务")?;
            }
            IndexJobStatus::Failed | IndexJobStatus::Cancelled => {
                let coordinator = self.visual_jobs().context("视觉任务协调器不可用")?;
                coordinator
                    .retry(&job_id)
                    .await
                    .context("无法重试视觉渲染任务")?;
            }
        }
        self.indexing.wake();
        Ok(job_id)
    }
}
