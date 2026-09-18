//! The limits one answer has to fit in: history, tools, citations and
//! context retries, plus the guidance prompts that go with them.

pub(crate) const MAX_HISTORY_MESSAGES: usize = 128;
pub(crate) const MAX_HISTORY_BYTES: usize = 512 * 1024;
pub(crate) const MAX_ANSWER_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_TOOL_CALLS_PER_TURN: usize = 8;
pub(crate) const MAX_TOOL_ARGUMENT_BYTES: usize = 256 * 1024;
pub(crate) const CITATION_MARKER_PREFIX: &str = "[[ngy-source:";
pub(crate) const CITATION_MARKER_SUFFIX: &str = "]]";
pub(crate) const MAX_CITATION_MARKERS: usize = 256;
pub(crate) const MAX_CONTEXT_RETRIES: usize = 3;
pub(crate) const TOOL_BUDGET_FINAL_GUIDANCE: &str = concat!(
    "\nThe host's tool-call budget for this question is exhausted. No tools are available. ",
    "Give the final answer now using the sources already returned or the host-validated ",
    "frozen selections. A tool_budget_exhausted result means that call was not executed ",
    "and supplies no source. If the available evidence is insufficient, say so plainly; ",
    "do not claim that an unexecuted search found or ruled out information. ",
    "Keep all existing citation and authorization rules."
);
// Host control data, not a successful retrieval result. Keep every assistant
// call paired even when a batch contains more calls than the remaining budget.
pub(crate) const TOOL_BUDGET_EXHAUSTED_RESULT: &str = r#"{"error":"tool_budget_exhausted","message":"This call was not executed. No tool calls remain; answer using the evidence already available and state any limitations."}"#;
pub(crate) const TOOL_ARGUMENT_RECOVERY_GUIDANCE: &str = concat!(
    "\nThe previous generation was rejected because tool arguments were incomplete JSON. ",
    "Generate at most one tool call at a time, with a short, complete JSON object matching ",
    "its schema. Close every string, array and object. For read_passages, use passage_ids ",
    "containing only exact passage_id values already returned by search_books; do not use ",
    "citation_id or book titles as passage IDs. If no passage IDs have been served, use ",
    "search_books first. Do not copy source text into arguments. Keep all existing source ",
    "citation and authorization rules."
);
