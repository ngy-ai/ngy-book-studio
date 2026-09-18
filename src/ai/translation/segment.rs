//! One stored translation segment.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranslationSegment {
    pub source: String,
    pub translated: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredTranslation {
    pub execution_identity: String,
    pub segments: Vec<TranslationSegment>,
}
