//! Integration tests of translation block extraction, reruns and the
//! read-only block inspector.

use super::*;

use std::sync::{Arc, Mutex, atomic::Ordering};

use crate::{ai::OpenAiCompatibleProvider, db, document::BlockDocument};

mod blocks_test;

mod inspection_test;

mod rerun_test;
