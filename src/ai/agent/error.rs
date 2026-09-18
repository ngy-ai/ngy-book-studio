//! The read-only agent error type.

use super::*;

pub type AgentResult<T> = std::result::Result<T, AgentError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentError {
    InvalidConfiguration(String),
    InvalidArguments(String),
    ScopeViolation,
    LimitExceeded(&'static str),
    ToolTimeout,
    Backend(String),
    StreamProtocol(String),
    UnknownCitation(String),
}

impl fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration(message) => {
                write!(formatter, "invalid agent configuration: {message}")
            }
            Self::InvalidArguments(message) => {
                write!(formatter, "invalid tool arguments: {message}")
            }
            Self::ScopeViolation => {
                formatter.write_str("requested book is outside the host-authorized scope")
            }
            Self::LimitExceeded(limit) => write!(formatter, "agent {limit} limit exceeded"),
            Self::ToolTimeout => formatter.write_str("agent tool call timed out"),
            Self::Backend(message) => write!(formatter, "agent backend failed: {message}"),
            Self::StreamProtocol(message) => {
                write!(formatter, "invalid tool-call stream: {message}")
            }
            Self::UnknownCitation(id) => write!(formatter, "unknown or unserved citation: {id}"),
        }
    }
}

impl Error for AgentError {}
