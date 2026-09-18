//! The severity levels a background job log entry can carry.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobLogLevel {
    Info,
    Warning,
    Error,
}

impl JobLogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Warning => "WARN",
            Self::Error => "ERROR",
        }
    }
}
