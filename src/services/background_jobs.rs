//! The service-side view of background jobs: listing, logs and control.

use super::*;

impl AppServices {
    /// Loads the complete persisted task history for the requested books on
    /// the application runtime. The scope is explicit and an empty scope never
    /// turns into an unbounded query.
    pub async fn background_jobs_for_books(
        &self,
        mut book_ids: Vec<String>,
    ) -> Result<Vec<BackgroundJobSnapshot>> {
        book_ids.retain(|book_id| !book_id.trim().is_empty());
        book_ids.sort();
        book_ids.dedup();
        if book_ids.is_empty() {
            return Ok(Vec::new());
        }

        let db_path = self.db_path.clone();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let conn = db::open_conn(&db_path)?;
                let mut snapshots = Vec::new();
                for book_id in &book_ids {
                    for job in db::index_jobs::list_for_book(&conn, book_id)? {
                        snapshots.push(background_job_snapshot(&conn, job)?);
                    }
                }
                snapshots.sort_by(|left, right| {
                    left.book_id
                        .cmp(&right.book_id)
                        .then_with(|| {
                            background_kind_order(&left.kind)
                                .cmp(&background_kind_order(&right.kind))
                        })
                        .then_with(|| left.id.cmp(&right.id))
                });
                Ok(snapshots)
            })
            .await
            .context("后台任务查询线程异常退出")?
    }

    /// Reads the most recent 500 diagnostic entries after checking the same
    /// explicit book scope used by the task list. An empty scope grants no
    /// access, and deleted jobs cannot expose leftover diagnostic history.
    pub async fn background_job_logs(
        &self,
        job_id: String,
        book_ids: Vec<String>,
    ) -> Result<BackgroundJobLogSnapshot> {
        ensure!(!job_id.trim().is_empty(), "后台任务 ID 不能为空");
        let db_path = self.db_path.clone();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let conn = db::open_conn(&db_path)?;
                let job = db::index_jobs::get(&conn, &job_id)?.context("后台任务已不存在")?;
                ensure!(
                    book_ids.iter().any(|book_id| book_id == &job.book_id),
                    "后台任务不在当前图书范围内"
                );
                crate::ai::job_diagnostics::JobDiagnosticStore::for_database(&db_path)?
                    .read(&job_id)
            })
            .await
            .context("后台任务日志查询线程异常退出")?
    }

    /// Structural text-block list of one whole-book translation task, used by
    /// the task window to show which blocks are done, in flight, or pending.
    /// The scope check mirrors the task list and logs: an empty scope grants no
    /// access, and a task outside the current scope is rejected. Parsing the
    /// pinned revision is expensive, so callers should cache the result per
    /// task instead of polling it.
    ///
    /// Reading the blocks touches the database and, for EPUB sources, the blob
    /// store, so the work runs on the application I/O runtime: callers such as
    /// the task window await this from a GPUI callback, which is not a Tokio
    /// context of its own.
    pub async fn background_job_translation_blocks(
        &self,
        job_id: String,
        book_ids: Vec<String>,
    ) -> Result<TranslationBlockList> {
        ensure!(!job_id.trim().is_empty(), "后台任务 ID 不能为空");
        let indexing = Arc::clone(&self.indexing);
        let db_path = self.db_path.clone();
        let scope_job_id = job_id.clone();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let conn = db::open_conn(&db_path)?;
                let job = db::index_jobs::get(&conn, &scope_job_id)?.context("后台任务已不存在")?;
                ensure!(
                    book_ids.iter().any(|book_id| book_id == &job.book_id),
                    "后台任务不在当前图书范围内"
                );
                Ok(())
            })
            .await
            .context("后台任务明细查询线程异常退出")??;
        self.runtime
            .spawn(async move { indexing.translation_blocks(&job_id).await })
            .await
            .context("后台任务明细线程异常退出")?
    }

    /// Reads one text block of a whole-book translation task together with the
    /// single-block request body the worker sends when the block goes out on
    /// its own, for the task window's debug view. Same access rule as
    /// [`Self::background_job_translation_blocks`]:
    /// the task must belong to the current book scope. Rebuilding the body
    /// re-parses the pinned revision and reads `book_sources`, so it runs on the
    /// application I/O runtime rather than in the GPUI callback.
    pub async fn background_job_translation_block_detail(
        &self,
        job_id: String,
        ordinal: usize,
        book_ids: Vec<String>,
    ) -> Result<TranslationBlockDetail> {
        ensure!(!job_id.trim().is_empty(), "后台任务 ID 不能为空");
        let indexing = Arc::clone(&self.indexing);
        let db_path = self.db_path.clone();
        let scope_job_id = job_id.clone();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let conn = db::open_conn(&db_path)?;
                let job = db::index_jobs::get(&conn, &scope_job_id)?.context("后台任务已不存在")?;
                ensure!(
                    book_ids.iter().any(|book_id| book_id == &job.book_id),
                    "后台任务不在当前图书范围内"
                );
                Ok(())
            })
            .await
            .context("后台任务明细查询线程异常退出")??;
        self.runtime
            .spawn(async move { indexing.translation_block_detail(&job_id, ordinal).await })
            .await
            .context("后台任务明细线程异常退出")?
    }

    /// 重新向模型请求一个文本块并返回它的原始回答，供任务窗口显示失败块的
    /// 「模型响应」。
    ///
    /// 与其它查看入口不同，这一条**会真的发一次模型请求**：任务诊断日志刻意只保存
    /// 固定分类，所以想知道模型当时答了什么，唯一办法就是拿同一份冻结输入再问一次。
    /// 因此它必须由用户的明确操作触发，**不得放进任何轮询**，作用域校验与其它查看入口
    /// 完全一致（任务必须属于当前图书范围）。请求本身可能很慢，所以走应用 I/O runtime。
    /// 除这一次外部请求外它是只读的：不推进游标、不写译文、不记诊断，回答只回给窗口。
    pub async fn background_job_translation_block_response(
        &self,
        job_id: String,
        ordinal: usize,
        book_ids: Vec<String>,
    ) -> Result<TranslationBlockProbe> {
        ensure!(!job_id.trim().is_empty(), "后台任务 ID 不能为空");
        let indexing = Arc::clone(&self.indexing);
        let db_path = self.db_path.clone();
        let scope_job_id = job_id.clone();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let conn = db::open_conn(&db_path)?;
                let job = db::index_jobs::get(&conn, &scope_job_id)?.context("后台任务已不存在")?;
                ensure!(
                    book_ids.iter().any(|book_id| book_id == &job.book_id),
                    "后台任务不在当前图书范围内"
                );
                Ok(())
            })
            .await
            .context("后台任务明细查询线程异常退出")??;
        self.runtime
            .spawn(async move {
                indexing
                    .probe_translation_block_response(&job_id, ordinal)
                    .await
            })
            .await
            .context("后台任务明细查询线程异常退出")?
    }

    /// Re-runs the named text blocks of a translation task.
    ///
    /// The blocks come from the task window's inspector, so they are ordinals of
    /// the pinned revision. Same access rule as the inspection reads: the task
    /// must belong to the current book scope. An empty set is rejected here so a
    /// UI bug cannot requeue a whole task by accident.
    pub async fn background_job_retry_translation_blocks(
        &self,
        job_id: String,
        ordinals: Vec<usize>,
        book_ids: Vec<String>,
    ) -> Result<bool> {
        ensure!(!job_id.trim().is_empty(), "后台任务 ID 不能为空");
        ensure!(!ordinals.is_empty(), "没有可重试的文本块");
        let indexing = Arc::clone(&self.indexing);
        let db_path = self.db_path.clone();
        let scope_job_id = job_id.clone();
        self.runtime
            .handle()
            .spawn_blocking(move || {
                let conn = db::open_conn(&db_path)?;
                let job = db::index_jobs::get(&conn, &scope_job_id)?.context("后台任务已不存在")?;
                ensure!(
                    book_ids.iter().any(|book_id| book_id == &job.book_id),
                    "后台任务不在当前图书范围内"
                );
                Ok(())
            })
            .await
            .context("后台任务重试线程异常退出")??;
        self.runtime
            .spawn(async move { indexing.retry_translation_blocks(&job_id, ordinals).await })
            .await
            .context("后台任务重试线程异常退出")?
    }

    /// Applies one state transition using the coordinator that owns the job
    /// kind. In particular, a running visual renderer must receive its in-memory
    /// control signal in addition to the durable SQLite flag.
    pub async fn control_background_job(
        &self,
        job_id: String,
        action: BackgroundJobAction,
    ) -> Result<bool> {
        ensure!(!job_id.trim().is_empty(), "后台任务 ID 不能为空");
        let indexing = Arc::clone(&self.indexing);
        let visual_jobs = self.visual_jobs();
        self.runtime
            .spawn(async move {
                let job = indexing
                    .job(&job_id)
                    .await?
                    .with_context(|| format!("后台任务已不存在：{job_id}"))?;
                match job.kind.as_str() {
                    "visual_render" => {
                        let coordinator = visual_jobs.context("视觉任务协调器不可用")?;
                        match action {
                            BackgroundJobAction::Pause => coordinator.pause(&job_id).await,
                            BackgroundJobAction::Resume => coordinator.resume(&job_id).await,
                            BackgroundJobAction::Retry => coordinator.retry(&job_id).await,
                            BackgroundJobAction::Cancel => coordinator.cancel(&job_id).await,
                            // Only translation has a re-run; a page render is
                            // rebuilt by re-importing a revision.
                            BackgroundJobAction::Retranslate => Ok(false),
                        }
                    }
                    "translation" => match action {
                        BackgroundJobAction::Pause => indexing.pause(&job_id).await,
                        BackgroundJobAction::Resume => indexing.resume(&job_id).await,
                        BackgroundJobAction::Retry => indexing.retry(&job_id).await,
                        BackgroundJobAction::Cancel => indexing.cancel(&job_id).await,
                        BackgroundJobAction::Retranslate => indexing.retranslate(&job_id).await,
                    },
                    "embedding" | "vision" => match action {
                        BackgroundJobAction::Pause => indexing.pause(&job_id).await,
                        BackgroundJobAction::Resume => indexing.resume(&job_id).await,
                        BackgroundJobAction::Retry => indexing.retry(&job_id).await,
                        BackgroundJobAction::Cancel => indexing.cancel(&job_id).await,
                        BackgroundJobAction::Retranslate => Ok(false),
                    },
                    other => anyhow::bail!("不支持控制后台任务类型：{other}"),
                }
            })
            .await
            .context("后台任务控制线程异常退出")?
    }
}
