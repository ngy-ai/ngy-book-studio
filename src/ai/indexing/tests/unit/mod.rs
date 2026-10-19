//! Unit tests: the pure request/response shapes around vision and translation.

use super::*;

use crate::{
    ai::job_diagnostics::{JobLogErrorKind, classify_error},
    ai::translation::{TranslationContext, TranslationSource},
    ai::{ChatMessage, MessageContent, ReasoningEffort},
    document::NormalizedRect,
};

mod prompt_test;

mod protocol_test;

mod request_test;
