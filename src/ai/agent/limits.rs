//! The budgets one agent run has to fit in.

use super::*;

#[derive(Clone, Debug)]
pub struct AgentLimits {
    /// Maximum individual tool calls per question, including invalid arguments.
    /// The historical name does not mean model responses: a response may ask
    /// for several calls, and each consumes this same budget.
    pub max_tool_rounds: usize,
    pub max_results_per_call: usize,
    pub max_context_bytes: usize,
    pub tool_timeout: Duration,
    pub max_snapshots: usize,
    pub max_outline_depth: usize,
    pub max_outline_nodes: usize,
}

impl Default for AgentLimits {
    fn default() -> Self {
        Self {
            max_tool_rounds: 6,
            max_results_per_call: 12,
            max_context_bytes: 96 * 1024,
            tool_timeout: Duration::from_secs(20),
            max_snapshots: 16,
            max_outline_depth: 8,
            max_outline_nodes: 256,
        }
    }
}

impl AgentLimits {
    pub fn validate(&self) -> AgentResult<()> {
        if !(1..=32).contains(&self.max_tool_rounds) {
            return Err(AgentError::InvalidConfiguration(
                "max_tool_rounds must be between 1 and 32".to_string(),
            ));
        }
        if !(1..=100).contains(&self.max_results_per_call) {
            return Err(AgentError::InvalidConfiguration(
                "max_results_per_call must be between 1 and 100".to_string(),
            ));
        }
        if !(1024..=16 * 1024 * 1024).contains(&self.max_context_bytes) {
            return Err(AgentError::InvalidConfiguration(
                "max_context_bytes must be between 1024 and 16777216".to_string(),
            ));
        }
        if self.tool_timeout.is_zero() || self.tool_timeout > Duration::from_secs(600) {
            return Err(AgentError::InvalidConfiguration(
                "tool_timeout must be between 1ns and 600s".to_string(),
            ));
        }
        if !(1..=64).contains(&self.max_snapshots) {
            return Err(AgentError::InvalidConfiguration(
                "max_snapshots must be between 1 and 64".to_string(),
            ));
        }
        if !(1..=16).contains(&self.max_outline_depth) {
            return Err(AgentError::InvalidConfiguration(
                "max_outline_depth must be between 1 and 16".to_string(),
            ));
        }
        if !(1..=4096).contains(&self.max_outline_nodes) {
            return Err(AgentError::InvalidConfiguration(
                "max_outline_nodes must be between 1 and 4096".to_string(),
            ));
        }
        Ok(())
    }
}
