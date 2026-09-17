//! The envelope format: bounded reads and the identity checks that decide
//! whether a payload may be trusted.

use super::*;

pub(crate) fn encode(payload: &RecordPayload) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(payload)?;
    let envelope = RecordEnvelope {
        format_version: 1,
        digest: blake3::hash(&bytes).to_hex().to_string(),
        payload: payload.clone(),
    };
    let encoded = serde_json::to_vec_pretty(&envelope)?;
    ensure!(
        encoded.len() <= MAX_RECORD_BYTES,
        "学习档案超过 32 MiB，请点击新一轮归档后继续"
    );
    Ok(encoded)
}

#[cfg(test)]
pub(crate) fn decode(bytes: &[u8]) -> Result<RecordPayload> {
    decode_for(bytes, 1)
}

pub(crate) fn decode_for(bytes: &[u8], chapter: u8) -> Result<RecordPayload> {
    validate_chapter(chapter)?;
    ensure!(bytes.len() <= MAX_RECORD_BYTES, "学习档案超过 32 MiB");
    let envelope: RecordEnvelope = serde_json::from_slice(bytes).context("学习档案格式无效")?;
    ensure!(envelope.format_version == 1, "不支持此学习档案版本");
    let payload = envelope.payload;
    ensure!(
        payload.course_id == chapter_course_id(chapter) && payload.course_version == COURSE_VERSION,
        "学习档案不属于当前章节，或课程版本不匹配"
    );
    ensure!(
        blake3::hash(&serde_json::to_vec(&payload)?)
            .to_hex()
            .as_str()
            == envelope.digest,
        "学习档案摘要不匹配，文件可能已损坏"
    );
    payload.workspace.validate_for(chapter)?;
    ensure!(
        payload.attempts.len() <= MAX_ATTEMPTS,
        "学习档案运行记录过多"
    );
    let mut seen = std::collections::HashSet::new();
    for attempt in &payload.attempts {
        ensure!(
            valid_id(&attempt.id) && seen.insert(&attempt.id),
            "学习档案运行标识无效或重复"
        );
        attempt.workspace.validate_for(chapter)?;
        ensure!(
            attempt.created_at.len() <= 64 && attempt.created_at.parse::<u64>().is_ok(),
            "学习记录时间无效"
        );
        ensure!(
            blake3::hash(attempt.workspace.code().as_bytes())
                .to_hex()
                .as_str()
                == attempt.code_digest,
            "首次代码快照与摘要不一致"
        );
        ensure!(
            serde_json::to_vec(&attempt.report)?.len() <= MAX_REPORT_BYTES,
            "运行报告过大"
        );
        ensure!(
            attempt.report.get("run_id").and_then(Value::as_str) == Some(&attempt.id),
            "运行报告标识不匹配"
        );
        validate_attempt_identity(&attempt.report, &attempt.workspace, chapter, true)?;
    }
    Ok(payload)
}

pub(crate) fn validate_attempt_identity(
    report: &Value,
    workspace: &LearningWorkspace,
    chapter: u8,
    loading: bool,
) -> Result<()> {
    // Existing first-chapter interrupted attempts predate request metadata.
    // Read those exact records without rewriting the file or weakening new
    // completed reports, later chapters, or the live host protocol.
    if loading
        && chapter == 1
        && report.get("request").is_none()
        && report
            .pointer("/outcome/stop_reason")
            .and_then(Value::as_str)
            == Some("run_not_committed")
        && report.get("passed").and_then(Value::as_bool) == Some(false)
    {
        return Ok(());
    }
    ensure!(
        report.pointer("/request/course_id").and_then(Value::as_str)
            == Some(chapter_course_id(chapter))
            && report
                .pointer("/request/course_version")
                .and_then(Value::as_str)
                == Some(COURSE_VERSION),
        "运行报告课程身份不匹配"
    );
    let reported_chapter = report.pointer("/request/chapter");
    ensure!(
        (chapter == 1 && reported_chapter.is_none())
            || reported_chapter.and_then(Value::as_u64) == Some(u64::from(chapter)),
        "运行报告章节不匹配"
    );
    ensure!(
        report
            .pointer("/request/implementation")
            .and_then(Value::as_str)
            == Some(workspace.implementation.as_str())
            && report.pointer("/request/task_id").and_then(Value::as_str)
                == Some(workspace.scenario.as_str()),
        "运行报告任务与首次作答不匹配"
    );
    if chapter > 1 {
        ensure!(
            report
                .pointer("/request/rules_version")
                .and_then(Value::as_str)
                == Some("1.0.0")
                && report
                    .pointer("/request/task_version")
                    .and_then(Value::as_str)
                    == Some("1.0.0")
                && report
                    .pointer("/request/model_mode")
                    .and_then(Value::as_str)
                    == Some("scripted"),
            "运行报告任务版本或模型模式不匹配"
        );
    }
    Ok(())
}

pub(crate) fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let file = fs::File::open(path).context("无法读取学习档案")?;
    let mut bytes = vec![];
    file.take((MAX_RECORD_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .context("学习档案读取失败")?;
    ensure!(bytes.len() <= MAX_RECORD_BYTES, "学习档案超过 32 MiB");
    Ok(bytes)
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("学习档案路径无效")?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).context("无法创建学习档案临时文件")?;
    temporary.write_all(bytes).context("无法写入学习档案")?;
    temporary.as_file().sync_all().context("无法同步学习档案")?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .context("无法提交学习档案")?;
    Ok(())
}
