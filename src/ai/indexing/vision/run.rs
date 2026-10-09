//! Running a vision pass over the pending pages.

use super::*;

pub(crate) async fn run_vision(
    inner: &Arc<IndexingInner>,
    job: &db::index_jobs::IndexJob,
    models: &ModelServices,
) -> Result<RunOutcome> {
    let mut cursor = JobCursor::from_job(
        job,
        &models.config.vision_model,
        Some(&models.config.vision_execution_identity),
    )?;
    if !source_is_current(inner, job, cursor.revision).await? {
        return Ok(RunOutcome::Cancelled(
            cursor,
            Some("图书已发布更新版本，任务结果已丢弃".to_string()),
        ));
    }
    persist_running_cursor(inner, job, &cursor).await?;

    let source_id = cursor.source_id.clone();
    let offset = cursor.next_ordinal;
    let db_path = inner.db_path.clone();
    let first = run_db(db_path, move |conn| {
        db::visual_pages::list_batch_for_source(&conn, &source_id, offset, VISION_PAGE_BATCH_SIZE)
    })
    .await?;
    if first.is_empty() && cursor.next_ordinal == 0 {
        return Ok(RunOutcome::WaitingForPages(cursor));
    }

    let mut pending = first;
    // 本次运行实际使用的 provider：默认是领取时的那份快照，用户保存过新配置之后换成新的
    // （见下面每一页的重读）。
    let mut provider = Arc::clone(&models.vision_provider);
    loop {
        if let Some(outcome) = requested_outcome(inner, job, &cursor).await? {
            return Ok(outcome);
        }
        if !source_is_current(inner, job, cursor.revision).await? {
            return Ok(RunOutcome::Cancelled(
                cursor,
                Some("图书已发布更新版本，任务结果已丢弃".to_string()),
            ));
        }
        if pending.is_empty() {
            let source_id = cursor.source_id.clone();
            let first_stale = visual_chunk_ordinal(cursor.next_ordinal, 0)?;
            let db_path = inner.db_path.clone();
            let book_id = cursor.book_id.clone();
            let job_id = job.id.clone();
            let expected_cursor_json = cursor.encode()?;
            let published = run_db_mut(db_path, move |mut conn| {
                db::transactions::finish_visual_search_chunks_for_running_vision_job(
                    &mut conn,
                    &job_id,
                    &expected_cursor_json,
                    &book_id,
                    &source_id,
                    first_stale,
                )
            })
            .await?;
            if !published {
                return settle_interrupted_or_abandoned(inner, job).await;
            }
            if !enqueue_post_vision_embedding(inner, job, &cursor).await? {
                return settle_interrupted_or_abandoned(inner, job).await;
            }
            return Ok(RunOutcome::Succeeded(cursor));
        }

        let page = pending.remove(0);
        let page_started = Instant::now();
        record_index_event(
            inner,
            job,
            JobLogEvent::ItemStarted,
            JobLogMetrics {
                ordinal: Some(page.page_index as u64),
                ..JobLogMetrics::default()
            },
        );
        ensure!(
            page.page_index == cursor.next_ordinal,
            "视觉页面序号不连续，拒绝错误定位"
        );
        ensure!(
            page.document_revision == cursor.revision
                && !page.renderer.trim().is_empty()
                && !page.renderer_version.trim().is_empty()
                && !page.profile_id.trim().is_empty()
                && matches!(
                    page.fidelity.as_str(),
                    "normalized" | "structural" | "office_enhanced"
                ),
            "视觉页面渲染元数据与当前任务不匹配"
        );
        let page_locator = serde_json::from_str::<DocumentLocator>(&page.locator_json)
            .context("视觉页面 locator JSON 无效")?;
        page_locator
            .validate()
            .context("视觉页面 locator 不符合统一模型约束")?;
        ensure!(
            page_locator.book_id == cursor.book_id,
            "视觉页面 locator 与页面所属图书不匹配"
        );
        ensure!(
            page_locator.region.is_none(),
            "视觉页面基础 locator 不得预先包含识别区域"
        );

        if !visual_page_is_citation_eligible(&page, &page_locator) {
            let expected_page = u32::try_from(page.page_index)
                .ok()
                .and_then(|index| index.checked_add(1));
            ensure!(
                page.content_unit_id.is_none()
                    && page.renderer == "ngy-office-com-enhanced"
                    && page.fidelity == "office_enhanced"
                    && page.unit_revision == 0
                    && page_locator.unit_id
                        == crate::preview::office_preview_unit_id(
                            &cursor.book_id,
                            &cursor.source_id,
                        )
                    && page_locator.block_id.is_none()
                    && page_locator.text_range.is_none()
                    && matches!(
                        (page_locator.source.as_ref(), expected_page),
                        (
                            Some(SourceLocator::OfficeRenderedPage { page }),
                            Some(expected_page)
                        ) if *page == expected_page
                    ),
                "Office preview-only 视觉页面身份无效"
            );
            // OfficeRenderedPage is intrinsically a preview-only coordinate;
            // renderer metadata is persisted but not trusted to redefine that
            // locator type. Assigning its OCR to a proportional unit would
            // create a false search result and AI citation. Remove any stale
            // visual chunks from this page onward before advancing the durable
            // cursor; later exact pages, if any, will be rebuilt normally.
            let first_stale = visual_chunk_ordinal(page.page_index, 0)?;
            let db_path = inner.db_path.clone();
            let book_id = cursor.book_id.clone();
            let source_id = cursor.source_id.clone();
            let job_id = job.id.clone();
            let expected_cursor_json = cursor.encode()?;
            let published = run_db_mut(db_path, move |mut conn| {
                db::transactions::finish_visual_search_chunks_for_running_vision_job(
                    &mut conn,
                    &job_id,
                    &expected_cursor_json,
                    &book_id,
                    &source_id,
                    first_stale,
                )
            })
            .await?;
            if !published {
                return settle_interrupted_or_abandoned(inner, job).await;
            }
            cursor.next_ordinal += 1;
            persist_running_cursor(inner, job, &cursor).await?;
            record_index_event(
                inner,
                job,
                JobLogEvent::PreviewOnlySkipped,
                JobLogMetrics {
                    ordinal: Some(page.page_index as u64),
                    duration_ms: Some(page_started.elapsed().as_millis() as u64),
                    ..JobLogMetrics::default()
                },
            );

            let source_id = cursor.source_id.clone();
            let offset = cursor.next_ordinal;
            let db_path = inner.db_path.clone();
            pending = run_db(db_path, move |conn| {
                db::visual_pages::list_batch_for_source(
                    &conn,
                    &source_id,
                    offset,
                    VISION_PAGE_BATCH_SIZE,
                )
            })
            .await?;
            continue;
        }

        let unit_id = page
            .content_unit_id
            .clone()
            .context("视觉页面缺少内容单元，无法建立可点击引用")?;
        ensure!(
            page_locator.unit_id == unit_id,
            "视觉页面 locator 与页面所属内容单元不匹配"
        );

        let object_key = BlobKey::parse(&page.object_key)?;
        let db_path = inner.db_path.clone();
        let object_key_text = page.object_key.clone();
        let metadata = run_db(db_path, move |conn| {
            db::blobs::get(&conn, &object_key_text)?.context("视觉页面对象元数据不存在")
        })
        .await?;
        ensure!(
            metadata.byte_len <= MAX_VISION_IMAGE_BYTES as u64,
            "视觉页面超过 {} 字节上限",
            MAX_VISION_IMAGE_BYTES
        );
        ensure!(
            is_supported_image_media_type(&metadata.media_type),
            "视觉页面 MIME 不受支持：{}",
            metadata.media_type
        );
        let bytes = inner.blobs.get(&object_key).await?;
        ensure!(
            bytes.len() as u64 == metadata.byte_len
                && blake3::hash(&bytes).to_hex().as_str() == metadata.hash,
            "视觉页面对象与数据库元数据不一致"
        );

        let image_url = format!(
            "data:{};base64,{}",
            metadata.media_type,
            BASE64_STANDARD.encode(&bytes)
        );
        // 用户在这次运行期间保存过 Endpoint/密钥时，`inner.models` 已经是新的，而这次运行
        // 手里还是领取时的快照（同类现场 2026-09-24：只换 key 时任务行不会被重置）。本页起
        // 换用新的 provider：模型与执行身份都一致才换 —— 换了身份的那次重配置会把任务行整个
        // 重置，本次运行随后会被判成已被替换。
        let live = inner.models_snapshot();
        let same_engine = live.config.vision_model == models.config.vision_model
            && live.config.vision_execution_identity == models.config.vision_execution_identity;
        if same_engine && !Arc::ptr_eq(&live.vision_provider, &provider) {
            tracing::info!(
                target: "ngy_ai",
                stage = "vision_provider_refreshed",
                page_index = page.page_index,
                model = %crate::ai::ai_diagnostics::safe_label(&models.config.vision_model),
                "Vision requests will use the reconfigured endpoint and key"
            );
            provider = Arc::clone(&live.vision_provider);
        }
        let request = vision_request(&models.config.vision_model, image_url);
        record_index_event(
            inner,
            job,
            JobLogEvent::ModelRequested,
            JobLogMetrics {
                ordinal: Some(page.page_index as u64),
                ..JobLogMetrics::default()
            },
        );
        let stream =
            match await_provider_step(inner, job, &cursor, provider.chat_stream(request)).await? {
                Controlled::Value(stream) => stream,
                Controlled::Interrupted(outcome) => return Ok(outcome),
            };
        let response = match collect_vision_response(inner, job, &cursor, stream).await? {
            Controlled::Value(response) => response,
            Controlled::Interrupted(outcome) => return Ok(outcome),
        };
        record_index_event(
            inner,
            job,
            JobLogEvent::ModelCompleted,
            JobLogMetrics {
                ordinal: Some(page.page_index as u64),
                response_bytes: Some(response.len() as u64),
                duration_ms: Some(page_started.elapsed().as_millis() as u64),
                ..JobLogMetrics::default()
            },
        );
        let output = parse_vision_output(&response)?;
        let region_count = output.regions.len();
        let first_page_ordinal = visual_chunk_ordinal(page.page_index, 0)?;
        let created_at = unix_timestamp()?;
        let mut chunks = Vec::with_capacity(output.regions.len());
        for (region_index, region) in output.regions.into_iter().enumerate() {
            let body = visual_chunk_body(&region);
            let bounds = region.bounds();
            let locator = match bounds {
                Some(bounds) => page_locator.clone().with_region(bounds),
                None => page_locator.clone(),
            };
            chunks.push(db::search_chunks::SearchChunk {
                id: visual_chunk_id(&cursor.source_id, page.page_index, bounds),
                book_id: cursor.book_id.clone(),
                source_id: cursor.source_id.clone(),
                content_unit_id: unit_id.clone(),
                ordinal: visual_chunk_ordinal(page.page_index, region_index)?,
                heading: if bounds.is_some() {
                    "视觉区域内容".to_string()
                } else {
                    "视觉页面摘要".to_string()
                },
                token_count: body.chars().count().div_ceil(4),
                content_hash: blake3::hash(body.as_bytes()).to_hex().to_string(),
                body,
                locator_json: serde_json::to_string(&locator).context("无法序列化视觉区域定位")?,
                created_at,
            });
        }
        // Publish the complete region set atomically. The transaction also
        // removes extra regions and later pages from an earlier run while
        // preserving already completed earlier pages.
        let db_path = inner.db_path.clone();
        let book_id = cursor.book_id.clone();
        let source_id = cursor.source_id.clone();
        let job_id = job.id.clone();
        let expected_cursor_json = cursor.encode()?;
        let published = run_db_mut(db_path, move |mut conn| {
            db::transactions::replace_visual_search_chunks_for_running_vision_job(
                &mut conn,
                &job_id,
                &expected_cursor_json,
                &book_id,
                &source_id,
                first_page_ordinal,
                &chunks,
            )
        })
        .await?;
        if !published {
            return settle_interrupted_or_abandoned(inner, job).await;
        }
        cursor.next_ordinal += 1;
        persist_running_cursor(inner, job, &cursor).await?;
        record_index_event(
            inner,
            job,
            JobLogEvent::ItemSaved,
            JobLogMetrics {
                ordinal: Some(page.page_index as u64),
                actual_count: Some(region_count as u64),
                duration_ms: Some(page_started.elapsed().as_millis() as u64),
                ..JobLogMetrics::default()
            },
        );

        let source_id = cursor.source_id.clone();
        let offset = cursor.next_ordinal;
        let db_path = inner.db_path.clone();
        pending = run_db(db_path, move |conn| {
            db::visual_pages::list_batch_for_source(
                &conn,
                &source_id,
                offset,
                VISION_PAGE_BATCH_SIZE,
            )
        })
        .await?;
    }
}
