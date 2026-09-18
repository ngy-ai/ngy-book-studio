//! One stored log entry and its timestamp formatting.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundJobLogEntry {
    /// The closed event that produced this line. Callers that need to tell one
    /// kind of failure from another must match on this, never on `message`.
    pub event: JobLogEvent,
    pub timestamp_ms: u64,
    pub level: JobLogLevel,
    pub stage: String,
    pub message: String,
    pub metrics: JobLogMetrics,
}

impl BackgroundJobLogEntry {
    /// A stable, complete copy representation with an explicit UTC timestamp.
    pub fn format_line(&self) -> String {
        let mut line = format!(
            "{} [{}] [{}] {}",
            format_timestamp(self.timestamp_ms),
            self.level.as_str(),
            self.stage,
            self.message
        );
        for (label, value) in [
            ("run_id", self.metrics.run_id),
            ("ordinal", self.metrics.ordinal),
            ("total", self.metrics.total),
            ("attempt", self.metrics.attempt),
            ("request_attempt", self.metrics.request_attempt),
            ("duration_ms", self.metrics.duration_ms),
            ("response_bytes", self.metrics.response_bytes),
            ("http_status", self.metrics.http_status),
            ("json_line", self.metrics.json_line),
            ("json_column", self.metrics.json_column),
            ("expected_count", self.metrics.expected_count),
            ("actual_count", self.metrics.actual_count),
        ] {
            if let Some(value) = value {
                let _ = write!(line, " {label}={value}");
            }
        }
        if let Some(error_kind) = self.metrics.error_kind {
            // Serialization of a closed enum cannot expose an error body.
            if let Ok(label) = serde_json::to_string(&error_kind) {
                let _ = write!(
                    line,
                    " error_kind={}({})",
                    label.trim_matches('"'),
                    error_kind.label()
                );
            }
        }
        line
    }
}

pub(crate) fn format_timestamp(timestamp_ms: u64) -> String {
    let seconds = timestamp_ms / 1000;
    let days = (seconds / 86_400) as i64;
    // Civil date conversion from Unix days, independent of local timezone and
    // platform date APIs. The input is nonnegative and well within i64.
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let time = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{:03} UTC",
        time / 3600,
        time / 60 % 60,
        time % 60,
        timestamp_ms % 1000
    )
}
