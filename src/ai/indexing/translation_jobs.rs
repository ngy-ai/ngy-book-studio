//! Coordinator entry points that arrange translation jobs: configuration,
//! whole-task rerun and per-block retry.

use super::*;

impl IndexingCoordinator {
    /// Binds the chat provider used for whole-book translation and reconciles
    /// the durable translation jobs with the active target language and model.
    /// Passing `None` as the target language disables translation and cancels
    /// every active job without touching persisted rows.
    pub async fn configure_translation(
        &self,
        provider: Arc<dyn OpenAiCompatibleProvider>,
        model: String,
        execution_identity: String,
        target_language: Option<String>,
        auto_run: bool,
    ) -> Result<()> {
        let model = validated_model(model, "translation")?;
        let execution_identity = validated_execution_identity(execution_identity, "translation")?;
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
                inflight: Vec::new(),
                blocks_version: TRANSLATION_BLOCKS_VERSION,
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

    /// Re-runs only the requested blocks of a translation job.
    ///
    /// This is the repair path after a model failed a block: the run that
    /// skipped it reported `Succeeded` and moved the durable cursor to the end
    /// of the book, so nothing would ever revisit those blocks. Retrying the
    /// whole book here (`retranslate`) would throw away every good translation
    /// and re-pay for all of them; retrying just the failed ones preserves them
    /// and lets the user switch to a better model first.
    ///
    /// The blocks are pinned by their ordinal in the current revision and the
    /// request is only forwarded when the block actually has no stored
    /// translation right now, so a stale list cannot overwrite a good
    /// translation or a hand-edited one. An empty set is rejected — the caller
    /// has to name at least one block.
    pub async fn retry_translation_blocks(
        &self,
        job_id: &str,
        ordinals: Vec<usize>,
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
            let language = translation_target_language(&job)?;
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
                inflight: Vec::new(),
                blocks_version: TRANSLATION_BLOCKS_VERSION,
            };
            // Only blocks with no stored translation are really retried. A block
            // that already has one (translated by an earlier attempt, or edited
            // by hand) is dropped from the set instead of being overwritten.
            // The check is by ordinal at the pinned revision, because the worker
            // resolves a cache hit by content and would otherwise re-send a block
            // whose text happens to differ from the cached key.
            let stored =
                db::translations::list_for_source(&conn, &job.book_id, &language, source.revision)?;
            let mut present: HashSet<usize> = HashSet::new();
            for row in &stored {
                present.insert(row.ordinal as usize);
            }
            cursor
                .retry_ordinals
                .retain(|ordinal| !present.contains(ordinal));
            // Everything already translated: nothing to do, and the task must not
            // be requeued for a no-op run.
            let Some(next) = cursor.retry_ordinals.first().copied() else {
                return Ok(0);
            };
            cursor.next_ordinal = next;
            let now = unix_timestamp()?;
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
                JobLogEvent::RetryRequested,
                JobLogMetrics::default(),
            );
            self.inner.wake.notify_one();
        }
        Ok(changed == 1)
    }
}
