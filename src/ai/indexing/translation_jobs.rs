//! Coordinator entry points that arrange translation jobs: configuration,
//! whole-task rerun and per-block retry.

use super::*;

impl IndexingCoordinator {
    /// Binds the provider used for whole-book translation and reconciles the
    /// durable translation jobs with the active target language and model.
    /// Passing `None` as the target language disables translation and cancels
    /// every active job without touching persisted rows.
    ///
    /// `generation` 是翻译请求自己的生成参数（默认温度 0）。它随每次请求发送，但**不是
    /// 执行身份的一部分**：调整它不会重建任务、不会让已翻好的译文失效，也不会把游标
    /// 退回书首。
    /// `batch_blocks` 是翻译端点「每请求文本块数」的**解析结果**（见
    /// `EndpointSettings::resolved_blocks_per_request`）：`1` 表示一块一请求，`> 1`
    /// 表示一次请求最多带走这么多块。与 `generation` 一样，它随每次请求生效但**不是**
    /// 执行身份的一部分：改它不会重建任务、不会让已翻好的译文失效。
    pub async fn configure_translation(
        &self,
        provider: Arc<dyn OpenAiCompatibleProvider>,
        model: String,
        execution_identity: String,
        generation: TranslationGenerationSettings,
        batch_blocks: usize,
        target_language: Option<String>,
        auto_run: bool,
    ) -> Result<()> {
        let model = validated_model(model, "translation")?;
        let execution_identity = validated_execution_identity(execution_identity, "translation")?;
        generation.validate()?;
        if let Some(language) = &target_language {
            ensure!(
                !language.trim().is_empty()
                    && language.trim() == language
                    && !language.chars().any(char::is_control),
                "翻译目标语言无效"
            );
        }
        let services = TranslationServices {
            provider,
            model,
            execution_identity,
            generation,
            // 端点策略给的最小是 1：装配点给 0 是 bug，而不是「关掉翻译」，所以直接
            // 收敛成一块一请求，绝不让 0 变成「一次装 0 块」。
            batch_blocks: batch_blocks.max(1),
            target_language,
            auto_run,
        };
        let _transition = self.inner.transitions.lock().await;
        {
            let mut models = self
                .inner
                .models
                .write()
                .unwrap_or_else(|error| error.into_inner());
            models.translation = Some(services.clone());
        }
        self.inner.models.clear_poison();
        let db_path = self.inner.db_path.clone();
        let reconcile = services.clone();
        let changed = run_db_on(self.inner.runtime.clone(), db_path, move |mut conn| {
            let now = unix_timestamp()?;
            db::transactions::reconfigure_translation_jobs(
                &mut conn,
                reconcile.target_language.as_deref(),
                &reconcile.model,
                &reconcile.execution_identity,
                reconcile.auto_run,
                now,
            )
        })
        .await?;
        self.inner
            .reconcile_translations
            .store(false, Ordering::Release);
        // 配置换了：丢弃 worker 手里可能还在推进的分步计划（它携带旧模型与执行
        // 身份）。这里已经按新配置整体对过一次账，下一次推进按现状重建即可。
        self.inner.clear_translation_reconcile();
        if changed != 0 {
            self.inner.wake.notify_one();
        }
        Ok(())
    }

    pub(crate) async fn request_control(
        &self,
        job_id: &str,
        request: ControlRequest,
    ) -> Result<bool> {
        let _transition = self.inner.transitions.lock().await;
        let diagnostic_id = job_id.to_string();
        let job_id = job_id.to_string();
        let db_path = self.inner.db_path.clone();
        let now = unix_timestamp()?;
        let changed = run_db_on(
            self.inner.runtime.clone(),
            db_path,
            move |conn| match request {
                ControlRequest::Pause => db::index_jobs::request_pause(&conn, &job_id, now),
                ControlRequest::Cancel => db::index_jobs::request_cancel(&conn, &job_id, now),
            },
        )
        .await?
            == 1;
        if changed {
            record_for_database(
                &self.inner.db_path,
                &diagnostic_id,
                match request {
                    ControlRequest::Pause => JobLogEvent::PauseRequested,
                    ControlRequest::Cancel => JobLogEvent::CancelRequested,
                },
                JobLogMetrics::default(),
            );
            self.wake();
        }
        Ok(changed)
    }

    /// Restarts one translation job from its first block. The persisted译文 of
    /// that book and language is discarded first, so a re-run cannot leave
    /// blocks that no longer exist. A superseded source or a non-translation
    /// job is rejected instead of modified.
    pub async fn retranslate(&self, job_id: &str) -> Result<bool> {
        let diagnostic_id = job_id.to_string();
        let translation = self
            .inner
            .models
            .read()
            .map_err(|_| anyhow::anyhow!("indexing model lock is poisoned"))?
            .translation
            .clone();
        let Some(translation) = translation else {
            return Ok(false);
        };
        let _transition = self.inner.transitions.lock().await;
        let job_id = job_id.to_string();
        let db_path = self.inner.db_path.clone();
        let changed = run_db_on(self.inner.runtime.clone(), db_path, move |conn| {
            let Some(job) = db::index_jobs::get(&conn, &job_id)? else {
                return Ok(0);
            };
            if job.kind != TRANSLATION_JOB_KIND {
                return Ok(0);
            }
            let language = translation_target_language(&job)?;
            let source_id = job.source_id.as_deref().context("翻译任务缺少来源")?;
            let source = db::book_sources::get(&conn, source_id)?.context("翻译任务来源不存在")?;
            let book = db::books::get(&conn, &job.book_id)?.context("翻译任务图书不存在")?;
            if book.revision != source.revision {
                return Ok(0);
            }
            let cursor = JobCursor {
                schema_version: 1,
                book_id: job.book_id.clone(),
                source_id: source_id.to_string(),
                revision: source.revision,
                kind: TRANSLATION_JOB_KIND.to_string(),
                model: Some(translation.model.clone()),
                execution_identity: Some(translation.execution_identity.clone()),
                input_execution_identity: None,
                next_ordinal: 0,
                retry_ordinals: Vec::new(),
                resume_ordinal: None,
                stops_after_targets: false,
                inflight: Vec::new(),
                blocks_version: TRANSLATION_BLOCKS_VERSION,
                priority: None,
                force_retry: false,
            };
            let now = unix_timestamp()?;
            db::translations::delete_for_retranslation(
                &conn,
                &job.book_id,
                &language,
                book.revision,
            )?;
            db::index_jobs::reset_reconfigured(
                &conn,
                &job_id,
                db::transactions::translation_initial_status(translation.auto_run),
                &cursor.encode()?,
                now,
            )
        })
        .await?;
        if changed != 0 {
            record_for_database(
                &self.inner.db_path,
                &diagnostic_id,
                JobLogEvent::RetranslateRequested,
                JobLogMetrics::default(),
            );
            self.inner.wake.notify_one();
        }
        Ok(changed == 1)
    }

    /// Re-runs one block named by its **block id** instead of its ordinal.
    ///
    /// 阅读端只知道块标识，而重试路径按序号工作，所以这里用与 worker 同一份提取规则
    /// 重新枚举当前修订的文本块，把标识换成序号再走 [`Self::retry_translation_blocks`]。
    /// 标识在当前正文里找不到（正文已改动、块被合并，或这一块本来就不在任务里）就明确
    /// 报错：随手挑一块重跑会写出另一块的译文。
    pub async fn retranslate_translation_block(
        &self,
        job_id: &str,
        content_unit_id: &str,
        block_id: &str,
    ) -> Result<bool> {
        ensure!(!content_unit_id.trim().is_empty(), "内容单元 ID 不能为空");
        ensure!(!block_id.trim().is_empty(), "文本块 ID 不能为空");
        let (book_id, source_id, revision) = self.translation_target(job_id).await?;
        let blocks = load_translation_blocks(&self.inner, &book_id, &source_id, revision).await?;
        let ordinal = blocks
            .iter()
            .find(|block| block.unit_id == content_unit_id && block.block_id == block_id)
            .map(|block| block.ordinal)
            .with_context(|| {
                format!("当前正文里找不到文本块 {block_id}，无法重新翻译（它可能已被改动）")
            })?;
        // force：这是用户看着这一块点下的「重新翻译」，命中缓存也要重新发给模型。
        // 任务窗口的「重试失败块」走 `retry_translation_blocks`（不强制）——那些块本来
        // 就没有译文，命中缓存是「别为同一块重复付费」的保护。
        self.plan_translation_range(
            job_id,
            vec![ordinal],
            TranslationRange::Retry { force: true },
        )
        .await
    }

    /// 「立即翻译本章」：把这一章的块排到最前翻完，然后回到原来的推进点继续整本。
    ///
    /// 与按块重试共用同一份规划（见 [`Self::plan_translation_range`]）：本章的块按序号交给
    /// walk、不做前置过滤，已有译文的块按缓存命中重挂、缺的才发请求。阅读端只知道章，
    /// 序号在这里由任务自己的文本块列表判定 —— 正文改动或块被合并导致本章找不到块就报错，
    /// 绝不顺手挑别的块充数。
    ///
    /// 正在运行的执行器拥有游标，所以调用方必须先让它落盘
    /// （`AppServices::translate_reader_chapter_now` 会请求暂停并等待），否则这次收窄会被
    /// 执行器随后的写入覆盖。
    pub async fn translate_translation_chapter(
        &self,
        job_id: &str,
        content_unit_id: &str,
    ) -> Result<bool> {
        ensure!(!content_unit_id.trim().is_empty(), "内容单元 ID 不能为空");
        let (book_id, source_id, revision) = self.translation_target(job_id).await?;
        let blocks = load_translation_blocks(&self.inner, &book_id, &source_id, revision).await?;
        let ordinals = blocks
            .iter()
            .filter(|block| block.unit_id == content_unit_id)
            .map(|block| block.ordinal)
            .collect::<Vec<_>>();
        ensure!(
            !ordinals.is_empty(),
            "当前正文里找不到这一章的文本块，无法立即翻译（它可能已被改动）"
        );
        self.plan_translation_range(job_id, ordinals, TranslationRange::Chapter)
            .await
    }

    /// 任务的书、来源与来源修订。
    ///
    /// 块序号只在这三者固定住的那一份修订里有意义，所以两条按序号规划翻译的路径都先解析
    /// 它们，再交给 worker 的同一份提取规则去枚举文本块。
    async fn translation_target(&self, job_id: &str) -> Result<(String, String, u64)> {
        let db_path = self.inner.db_path.clone();
        let lookup_id = job_id.to_string();
        run_db_on(self.inner.runtime.clone(), db_path, move |conn| {
            let job = db::index_jobs::get(&conn, &lookup_id)?.context("翻译任务已不存在")?;
            ensure!(job.kind == TRANSLATION_JOB_KIND, "该任务不是整本翻译");
            let source_id = job.source_id.clone().context("翻译任务缺少来源")?;
            let source = db::book_sources::get(&conn, &source_id)?.context("翻译任务来源不存在")?;
            Ok((job.book_id.clone(), source_id, source.revision))
        })
        .await
    }

    /// Re-runs only the requested blocks of a translation job.
    ///
    /// This is the repair path after a model failed a block: the run that
    /// skipped it reported `Succeeded` and moved the durable cursor to the end
    /// of the book, so nothing would ever revisit those blocks. Retrying the
    /// whole book here (`retranslate`) would throw away every good translation
    /// and re-pay for all of them; retrying just the failed ones preserves them
    /// and lets the user switch to a better model first.
    ///
    /// The blocks are pinned by their ordinal in the current revision and handed
    /// to the walk **unchanged**: whether one of them still needs a request is
    /// decided there, by the same content + model + execution identity rule the
    /// run itself uses. Guessing「已有译文」from the ordinal here makes a retry
    /// send nothing whenever that guess disagrees with the run (a row the reader
    /// cannot display, or one whose ordinal shifted after the block list
    /// changed). A block that does have a displayable translation is republished
    /// as a cache hit instead of being re-sent. An empty set is rejected — the
    /// caller has to name at least one block.
    pub async fn retry_translation_blocks(
        &self,
        job_id: &str,
        ordinals: Vec<usize>,
    ) -> Result<bool> {
        self.plan_translation_range(job_id, ordinals, TranslationRange::Retry { force: false })
            .await
    }

    /// 把一组序号收窄成一次运行，并按范围用途决定跑完之后去哪。
    ///
    /// `range` 决定目标块命中缓存时怎么办：阅读端的「重新翻译」强制重发（用户对着
    /// 这一块点的，译文不满意、或换了更好的模型想重做），其余入口保持缓存规则。
    /// 集合原样交给 walk，**不做前置过滤**：判定「这一块已经有译文」必须与 worker 的缓存
    /// 规则完全一致（单元内容键 + 模型 + 执行身份），而这里只有 ordinal 和译文行，凭它
    /// 判定会把阅读端其实看不到的行、或 ordinal 已经错位（块列表变过）的行当成「已有
    /// 译文」，于是用户点了「重试」却一个请求都不发 —— 日志里既没有请求体也没有响应
    /// （现场 2026-09-22）。交给 walk 之后两种情形都不会被误判：已有译文的块按缓存命中
    /// 重挂到当前块，其余块正常发请求；`retranslate`（整本重做）保留原来的删除语义。
    async fn plan_translation_range(
        &self,
        job_id: &str,
        ordinals: Vec<usize>,
        range: TranslationRange,
    ) -> Result<bool> {
        let diagnostic_id = job_id.to_string();
        let translation = self
            .inner
            .models
            .read()
            .map_err(|_| anyhow::anyhow!("indexing model lock is poisoned"))?
            .translation
            .clone();
        let Some(translation) = translation else {
            return Ok(false);
        };
        let mut retry_ordinals = ordinals;
        retry_ordinals.sort_unstable();
        retry_ordinals.dedup();
        if retry_ordinals.is_empty() {
            return Ok(false);
        }
        let first = *retry_ordinals.first().context("重试集合在去重后为空")?;
        let _transition = self.inner.transitions.lock().await;
        let job_id = job_id.to_string();
        let db_path = self.inner.db_path.clone();
        let changed = run_db_on(self.inner.runtime.clone(), db_path, move |conn| {
            let Some(job) = db::index_jobs::get(&conn, &job_id)? else {
                return Ok(0);
            };
            if job.kind != TRANSLATION_JOB_KIND {
                return Ok(0);
            }
            // 语言本身在这一路径上用不到：重试集合原样交给 walk，由它重新解析语言与
            // 缓存规则。保留这次解析，是因为任务 ID 里的语言属于任务身份的一部分，
            // 非法就该在这里拒绝，而不是留到 walk 里才失败。
            translation_target_language(&job)?;
            let source_id = job.source_id.as_deref().context("翻译任务缺少来源")?;
            let source = db::book_sources::get(&conn, source_id)?.context("翻译任务来源不存在")?;
            let book = db::books::get(&conn, &job.book_id)?.context("翻译任务图书不存在")?;
            if book.revision != source.revision {
                return Ok(0);
            }
            let mut cursor = JobCursor {
                schema_version: 1,
                book_id: job.book_id.clone(),
                source_id: source_id.to_string(),
                revision: source.revision,
                kind: TRANSLATION_JOB_KIND.to_string(),
                model: Some(translation.model.clone()),
                execution_identity: Some(translation.execution_identity.clone()),
                input_execution_identity: None,
                next_ordinal: first,
                retry_ordinals: retry_ordinals.clone(),
                resume_ordinal: None,
                stops_after_targets: false,
                inflight: Vec::new(),
                blocks_version: TRANSLATION_BLOCKS_VERSION,
                priority: None,
                force_retry: range.force_retry(),
            };
            let now = unix_timestamp()?;
            let last = cursor.retry_ordinals.last().copied().unwrap_or(first);
            match range {
                TranslationRange::Chapter => {
                    // 把推进点借给这一章之前先记下原地位置：最后一个目标块提交时 walk 回到
                    // 这里继续整本（`TranslationRun::commit`）。已经有一次收窄在等回来时保留
                    // 更早的那一个 —— 连着点两章不该把中间那一段丢掉。
                    let current = JobCursor::from_job(
                        &job,
                        &translation.model,
                        Some(&translation.execution_identity),
                    )?;
                    cursor.resume_ordinal = current.resume_ordinal.or(Some(current.next_ordinal));
                }
                TranslationRange::Retry { force: true } => {
                    // 逐块「重新翻译」只做用户点的那一块：walk 的窗口停在它之后，后来的
                    // 文本块既不会重挂，也不会被算进这一轮（现场 2026-10-19：点一块重译，
                    // 任务窗口里这一块之后的所有块都显示成了这一轮的范围）。
                    cursor.stops_after_targets = true;
                    // 目标块通常落在已处理区域（推进点之后）：跑完回到推进点，下一次运行
                    // 不会重扫中间已翻好的块。目标块本身还没翻过时推进点停在它之前，回到
                    // 那里才能把中间尚未翻译的块接着翻完；只有目标块正好是下一个未处理块
                    // 时才让它前进一格，避免下一次运行又从头处理同一块。
                    let current = JobCursor::from_job(
                        &job,
                        &translation.model,
                        Some(&translation.execution_identity),
                    )?;
                    cursor.resume_ordinal = current
                        .resume_ordinal
                        .or_else(|| (current.next_ordinal != last).then_some(current.next_ordinal));
                }
                // 任务窗口的「重试失败块」保持旧语义：集合原样交给 walk，跑完停在该组之后。
                TranslationRange::Retry { force: false } => {}
            }
            // 阅读端的两个入口（逐块「重新翻译」与「立即翻译本章」）对用户都是「现在就看我
            // 这里」：写下比既有插队请求更新的优先级，`list_queued` 会把它排到所有待执行
            // 任务之前。同一本书正在跑的任务由调用方先暂停，等它做完手上这一批让出执行器
            // 就轮到它；没有在跑的任务时，worker 的下一次领取就是它。
            if range.prioritizes() {
                cursor.priority = Some(now);
            }
            match range {
                TranslationRange::Retry { force } => tracing::info!(
                    target: "ngy_ai",
                    stage = "translation_retry_plan",
                    job_id = %job_id,
                    requested = cursor.retry_ordinals.len(),
                    next_ordinal = first,
                    last_ordinal = last,
                    force,
                    stops_after_targets = cursor.stops_after_targets,
                    resume_ordinal = ?cursor.resume_ordinal,
                    "Planned a per-block translation retry"
                ),
                TranslationRange::Chapter => tracing::info!(
                    target: "ngy_ai",
                    stage = "translation_chapter_plan",
                    job_id = %job_id,
                    requested = cursor.retry_ordinals.len(),
                    next_ordinal = first,
                    last_ordinal = last,
                    resume_ordinal = ?cursor.resume_ordinal,
                    "Planned a chapter-first translation jump"
                ),
            }
            db::index_jobs::reset_for_translation_block_retry(
                &conn,
                &job_id,
                &cursor.encode()?,
                now,
            )
        })
        .await?;
        if changed != 0 {
            record_for_database(
                &self.inner.db_path,
                &diagnostic_id,
                range.log_event(),
                JobLogMetrics::default(),
            );
            self.inner.wake.notify_one();
        }
        Ok(changed == 1)
    }
}

/// 一组收窄序号交给 walk 之后，跑完停在哪。
#[derive(Clone, Copy, PartialEq, Eq)]
enum TranslationRange {
    /// 按块重试（修失败块、阅读端逐块重翻）：停在集合之后，维持既有行为。
    ///
    /// `force` 说命中缓存的目标块要不要重发：阅读端的「重新翻译」为 `true`（用户对着
    /// 这一块点的，译文不满意或换了更好的模型想重做），任务窗口的「重试失败块」为
    /// `false`（那些块本来就没有译文，缓存命中是别重复付费的保护）。
    Retry { force: bool },
    /// 「立即翻译本章」：跑完回到借出推进点之前的位置继续整本，并把这本排到队首。
    Chapter,
}

impl TranslationRange {
    /// 这次收窄要不要让目标块越过缓存命中。
    fn force_retry(self) -> bool {
        match self {
            Self::Retry { force } => force,
            Self::Chapter => false,
        }
    }

    /// 这次收窄是不是阅读端「现在就要」的请求，要插到待执行队列最前。
    ///
    /// 逐块「重新翻译」与「立即翻译本章」的优先级规则相同；任务窗口的「重试失败块」
    /// 只是修复失败块，不抢正在跑的整本翻译。
    fn prioritizes(self) -> bool {
        match self {
            Self::Retry { force } => force,
            Self::Chapter => true,
        }
    }

    fn log_event(self) -> JobLogEvent {
        match self {
            Self::Retry { force: false } => JobLogEvent::RetryRequested,
            Self::Retry { force: true } => JobLogEvent::BlockRetranslateRequested,
            Self::Chapter => JobLogEvent::ChapterRequested,
        }
    }
}
