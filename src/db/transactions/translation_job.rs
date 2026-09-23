//! Reconfiguring the translation job and validating its cursor.

use super::*;

/// Reconciles the durable whole-book translation jobs with the active target
/// language, translation model and execution identity. One job per current
/// source and target language is retained; jobs for a superseded
/// language/source are cancelled. A changed engine keeps the durable position:
/// the rows that are already translated are restamped with the active identity
/// and the run continues from the first block that is still missing, so a
/// half-read book neither loses its译文 nor pays for the finished part twice.
/// Passing `None` disables translation and cancels every active job.
pub(crate) fn reconfigure_translation_jobs(
    conn: &mut Connection,
    target_language: Option<&str>,
    model: &str,
    execution_identity: &str,
    auto_run: bool,
    now: u64,
) -> Result<usize> {
    if let Some(language) = target_language {
        validate_index_model(language, "翻译目标语言")?;
    }
    validate_index_model(model, "对话")?;
    validate_index_model(execution_identity, "翻译任务执行身份")?;

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动翻译任务重配置事务")?;

    let mut desired: BTreeMap<String, BookSource> = BTreeMap::new();
    if let Some(language) = target_language {
        for source in book_sources::list_current(&tx)? {
            if !format_supports_translation(&source.format) {
                continue;
            }
            let book = books::get(&tx, &source.book_id)?.context("翻译任务图书不存在")?;
            if book.revision != source.revision {
                continue;
            }
            if language_matches_source(&book.language, language) {
                // The book already uses the target language; there is nothing
                // to translate and any previous job must be superseded.
                continue;
            }
            desired.insert(format!("translation:{}:{language}", source.id), source);
        }
    }

    let existing = index_jobs::list_by_kind(&tx, "translation")?;
    let mut changed = 0usize;
    for job in &existing {
        match desired.get(&job.id) {
            Some(source) => {
                let cursor = valid_translation_cursor(job, source);
                let current = cursor.as_ref().is_some_and(|cursor| {
                    cursor.model.as_deref() == Some(model)
                        && cursor.execution_identity.as_deref() == Some(execution_identity)
                });
                if current {
                    continue;
                }
                // A changed chat model or endpoint keeps the text that is already
                // translated: the persisted rows are restamped with the active
                // identity and only the missing blocks are queued again, so a
                // half-read book does not lose its译文. A row written by another
                // protocol version cannot be reused, and that case still discards
                // the book's译文 and starts over.
                let reusable = match target_language {
                    Some(language) => super::super::translations::adopt_execution_identity(
                        &tx,
                        &source.book_id,
                        language,
                        model,
                        execution_identity,
                    )?,
                    None => true,
                };
                let next_ordinal = if reusable {
                    // The block list only moves when the source revision changes.
                    // An identity change alone keeps the durable position, while
                    // a stale cursor still restarts from the first block.
                    cursor
                        .as_ref()
                        .map(|cursor| cursor.next_ordinal)
                        .unwrap_or_default()
                } else {
                    if let Some(language) = target_language {
                        super::super::translations::delete_for_retranslation(
                            &tx,
                            &source.book_id,
                            language,
                            source.revision,
                        )?;
                    }
                    0
                };
                // 位置必须连着它的「出处」一起走：`blocks_version` 是这一版块提取规则
                // 的记号，丢下它等于告诉执行器这个位置来自另一套规则（缺省 0 → 退回
                // 书首）。重配置不改块列表，所以这里只搬运原游标的那个值。
                let blocks_version = cursor.as_ref().map_or(0, |cursor| cursor.blocks_version);
                let cursor_json = translation_cursor_json(
                    source,
                    model,
                    execution_identity,
                    next_ordinal,
                    blocks_version,
                )?;
                let status = translation_initial_status(auto_run);
                if index_jobs::reset_reconfigured(&tx, &job.id, status, &cursor_json, now)? == 1 {
                    changed += 1;
                }
            }
            None => {
                if index_jobs::cancel_reconfigured(&tx, &job.id, None, now)? == 1 {
                    changed += 1;
                }
            }
        }
    }

    let existing_ids = existing
        .iter()
        .map(|job| job.id.as_str())
        .collect::<HashSet<_>>();
    for (id, source) in &desired {
        if existing_ids.contains(id.as_str()) {
            continue;
        }
        // 全新任务停在书首，位置与块版本都对不上任何已有译文：写 0（来源未知），
        // 执行器第一次领取时按当前版本规范化这一行。
        let cursor_json = translation_cursor_json(source, model, execution_identity, 0, 0)?;
        index_jobs::insert(
            &tx,
            &IndexJob {
                id: id.clone(),
                book_id: source.book_id.clone(),
                source_id: Some(source.id.clone()),
                kind: "translation".to_string(),
                status: translation_initial_status(auto_run),
                pause_requested: false,
                cancel_requested: false,
                attempts: 0,
                cursor_json,
                error: None,
                created_at: now,
                updated_at: now,
                started_at: None,
                finished_at: None,
            },
        )?;
        changed += 1;
    }

    tx.commit().context("无法提交翻译任务重配置事务")?;
    Ok(changed)
}

pub(crate) fn translation_initial_status(auto_run: bool) -> IndexJobStatus {
    if auto_run {
        IndexJobStatus::Queued
    } else {
        IndexJobStatus::Paused
    }
}

/// Encodes one translation job's cursor.
///
/// `blocks_version` 是 `next_ordinal` 的出处：这个位置是用哪一版文本块提取规则算出
/// 来的，只有执行器能判断它是否还指向同一段文字（`JobCursor::from_job`）。因此
/// **重建既有游标时必须传原游标里的那个值**，不能在这里写当前版本、更不能写缺省值
/// —— 重配置（换模型/端点）不改块列表，把它写成「来源未知」会让执行器把一次换模型
/// 读成「块列表变过」而把任务退回书首重扫（现场 2026-09-22：暂停在三千多块的任务
/// 换模型后「已处理」归零）。全新任务停在书首，位置与任何译文都对不上，写 0 即可。
pub(crate) fn translation_cursor_json(
    source: &BookSource,
    model: &str,
    execution_identity: &str,
    next_ordinal: usize,
    blocks_version: u32,
) -> Result<String> {
    let cursor = PersistedIndexCursor {
        schema_version: 1,
        book_id: source.book_id.clone(),
        source_id: source.id.clone(),
        revision: source.revision,
        kind: "translation".to_string(),
        model: Some(model.to_string()),
        execution_identity: Some(execution_identity.to_string()),
        input_execution_identity: None,
        next_ordinal,
        blocks_version,
    };
    serde_json::to_string(&cursor).context("无法序列化翻译任务游标")
}

pub(crate) fn valid_translation_cursor(
    job: &IndexJob,
    source: &BookSource,
) -> Option<PersistedIndexCursor> {
    let cursor = serde_json::from_str::<PersistedIndexCursor>(&job.cursor_json).ok()?;
    (job.kind == "translation"
        && job.book_id == source.book_id
        && job.source_id.as_deref() == Some(source.id.as_str())
        && cursor.schema_version == 1
        && cursor.book_id == source.book_id
        && cursor.source_id == source.id
        && cursor.revision == source.revision
        && cursor.kind == "translation")
        .then_some(cursor)
}

/// Whole-book translation targets reflowable prose only. Fixed-layout PDF and
/// Office spreadsheet/presentation sources are outside the current scope and
/// must not accumulate meaningless translation jobs.
pub(crate) fn format_supports_translation(format: &str) -> bool {
    matches!(format, "epub" | "mobi" | "azw" | "azw3" | "doc" | "docx")
}

/// Whether a book's declared language already satisfies the target. Compares
/// only the primary subtag so `en-US` matches `en` and `zh` matches `zh-Hans`.
pub(crate) fn language_matches_source(source: &Option<String>, target: &str) -> bool {
    pub(super) fn primary(tag: &str) -> String {
        tag.trim()
            .split(['-', '_'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    }
    match source {
        Some(source) => {
            let source = primary(source);
            !source.is_empty() && source == primary(target)
        }
        None => false,
    }
}
