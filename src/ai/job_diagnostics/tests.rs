//! Tests for the job diagnostics store.

use super::*;

fn fixture() -> (tempfile::TempDir, PathBuf, JobDiagnosticStore) {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("library.db");
    fs::write(&database, []).unwrap();
    let store = JobDiagnosticStore::for_database(&database).unwrap();
    (temp, database, store)
}

#[test]
fn records_survive_reopen_and_do_not_expose_input_or_error_strings() {
    let (_temp, database, store) = fixture();
    let secret = "book body api-key=secret https://private.example/token";
    store
        .record(
            secret,
            JobLogEvent::RunFailed,
            JobLogMetrics {
                run_id: Some(17),
                ordinal: Some(8),
                error_kind: Some(classify_error(&anyhow::anyhow!(secret.to_string()))),
                ..Default::default()
            },
        )
        .unwrap();
    drop(store);
    let reopened = JobDiagnosticStore::for_database(&database).unwrap();
    let log = reopened.read(secret).unwrap();
    assert_eq!(log.entries.len(), 1);
    assert_eq!(log.entries[0].event, JobLogEvent::RunFailed);
    let line = log.entries[0].format_line();
    assert!(line.contains("run_id=17 ordinal=8"));
    assert!(line.contains("error_kind=unknown(未归类的错误)"));
    assert!(!line.contains(secret));
    assert!(!String::from_utf8_lossy(&fs::read(reopened.path).unwrap()).contains(secret));
}

/// A failure category without a readable explanation is the exact gap this
/// table closes: the window can only show fixed text, never the error body.
#[test]
fn every_failure_category_has_a_distinct_readable_label() {
    let categories = [
        JobLogErrorKind::Provider,
        JobLogErrorKind::Database,
        JobLogErrorKind::Io,
        JobLogErrorKind::InvalidData,
        JobLogErrorKind::Timeout,
        JobLogErrorKind::Translation,
        JobLogErrorKind::ContextWindowExceeded,
        JobLogErrorKind::StreamProtocol,
        JobLogErrorKind::Cancelled,
        JobLogErrorKind::InvalidJson,
        JobLogErrorKind::IncompleteJson,
        JobLogErrorKind::InvalidSchema,
        JobLogErrorKind::IncompleteReasoning,
        JobLogErrorKind::AmbiguousJson,
        JobLogErrorKind::MissingJson,
        JobLogErrorKind::SegmentCountMismatch,
        JobLogErrorKind::UnknownSegmentId,
        JobLogErrorKind::DuplicateSegmentId,
        JobLogErrorKind::MissingSegmentId,
        JobLogErrorKind::EmptySegmentText,
        JobLogErrorKind::InvalidSegmentText,
        JobLogErrorKind::ResponseTruncated,
        JobLogErrorKind::Unknown,
    ];
    let mut labels = std::collections::BTreeSet::new();
    for category in categories {
        let label = category.label();
        assert!(!label.trim().is_empty(), "{category:?} 缺少说明");
        assert!(
            label
                .chars()
                .any(|character| ('\u{4e00}'..='\u{9fff}').contains(&character)),
            "{category:?} 的说明必须包含可读中文：{label}"
        );
        assert!(labels.insert(label), "{category:?} 的说明与其它分类重复");
    }
    // The classifier must only ever return a category covered by the table.
    assert_eq!(
        classify_error(&anyhow::anyhow!("boom")),
        JobLogErrorKind::Unknown
    );
}

#[test]
fn task_and_library_scopes_are_independent() {
    let (_first, database, first) = fixture();
    let (_second, _, second) = fixture();
    first
        .record("job-a", JobLogEvent::RunStarted, JobLogMetrics::default())
        .unwrap();
    assert_eq!(first.read("job-a").unwrap().entries.len(), 1);
    assert!(first.read("job-b").unwrap().entries.is_empty());
    assert!(second.read("job-a").unwrap().entries.is_empty());
    let sibling = database.with_file_name("another-library.db");
    fs::write(&sibling, []).unwrap();
    assert!(
        JobDiagnosticStore::for_database(&sibling)
            .unwrap()
            .read("job-a")
            .unwrap()
            .entries
            .is_empty()
    );
}

#[test]
fn retention_prunes_old_rows_atomically_and_bounds_the_whole_store() {
    let (_temp, _, store) = fixture();
    let mut conn = store.connect().unwrap();
    for ordinal in 0..8 {
        store
            .record_on(
                &mut conn,
                "job-a",
                JobLogEvent::ItemSaved,
                JobLogMetrics {
                    ordinal: Some(ordinal),
                    ..Default::default()
                },
                3,
                5,
            )
            .unwrap();
    }
    let first = store.read("job-a").unwrap();
    assert!(first.truncated);
    assert_eq!(first.entries.len(), 3);
    assert_eq!(first.entries[0].metrics.ordinal, Some(5));
    for _ in 0..3 {
        store
            .record_on(
                &mut conn,
                "job-b",
                JobLogEvent::RunStarted,
                JobLogMetrics::default(),
                3,
                5,
            )
            .unwrap();
    }
    let count: usize = conn
        .query_row("SELECT COUNT(*) FROM logs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 5);
    let first = store.read("job-a").unwrap();
    assert_eq!(first.entries.len(), 2);
    assert!(first.truncated);
    assert_eq!(first.entries[0].metrics.ordinal, Some(6));
    let max_pages: u64 = conn
        .pragma_query_value(None, "max_page_count", |row| row.get(0))
        .unwrap();
    assert_eq!(max_pages, MAX_DATABASE_PAGES);
}

#[test]
fn production_retention_returns_exactly_the_most_recent_500_rows() {
    let (_temp, _, store) = fixture();
    let mut conn = store.connect().unwrap();
    for ordinal in 0..=BACKGROUND_JOB_LOG_LIMIT {
        store
            .record_on(
                &mut conn,
                "job",
                JobLogEvent::ItemSaved,
                JobLogMetrics {
                    ordinal: Some(ordinal as u64),
                    ..Default::default()
                },
                BACKGROUND_JOB_LOG_LIMIT,
                BACKGROUND_JOB_LOG_TOTAL_LIMIT,
            )
            .unwrap();
    }
    let snapshot = store.read("job").unwrap();
    assert_eq!(snapshot.entries.len(), 500);
    assert!(snapshot.truncated);
    assert_eq!(snapshot.entries[0].metrics.ordinal, Some(1));
    assert_eq!(snapshot.entries[499].metrics.ordinal, Some(500));
}

#[test]
fn persisted_unknown_fields_are_rejected() {
    let (_temp, _, store) = fixture();
    store
        .record("job", JobLogEvent::RunStarted, JobLogMetrics::default())
        .unwrap();
    store
        .connect()
        .unwrap()
        .execute(
            "UPDATE logs SET metrics_json = ?1",
            [r#"{"response_text":"private response"}"#],
        )
        .unwrap();
    let error = store.read("job").unwrap_err();
    assert_eq!(error.to_string(), "任务日志字段无效");
    assert!(!format!("{error:#}").contains("response_text"));
    assert!(!format!("{error:#}").contains("private response"));
}

#[test]
fn copied_timestamps_include_readable_utc_date_and_milliseconds() {
    assert_eq!(format_timestamp(0), "1970-01-01 00:00:00.000 UTC");
    assert_eq!(
        format_timestamp(1_700_000_000_123),
        "2023-11-14 22:13:20.123 UTC"
    );
}
