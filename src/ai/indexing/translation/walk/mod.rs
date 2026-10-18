//! Planning and walking the parallel block window.
//!
//! - [`drive`] holds the main loop: commit in ordinal order, fill the window,
//!   wait for the next request, and publish the run's final outcome.
//! - [`window`] opens the requests of one batch and paces them.
//! - [`transitions`] takes the transition gate while the window keeps moving,
//!   absorbs one request's per-block results, and settles a stopped run.
//! - [`plan`] decides what one ordinal needs and keeps the per-run caches.
//!
//! The window is the only thing block-level concurrency changed: every block
//! still commits in ordinal order, so the durable cursor keeps the meaning it
//! always had (the first block the task has not finished) and a pause or cancel
//! simply drops whatever is in flight — those blocks are not committed and the
//! next run redoes them. Requests never borrow the cursor, which the walk keeps
//! advancing while they wait; they read the run's committed JSON instead,
//! through [`TranslationBlockScope`].

use super::*;

use crate::ai::translation::{
    TranslationBatchBudget, TranslationBatchPacker, estimate_batch_block,
};

mod drive;

mod plan;

mod transitions;

mod window;

pub(crate) use drive::walk_translation_blocks;

pub(crate) use plan::{
    block_skip_cause, keep_in_unit_cache, plan_translation_block, refreshed_translation_services,
};

pub(crate) use transitions::*;

pub(crate) use window::*;
