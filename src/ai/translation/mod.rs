//! Text-only translation protocol. The original document owns every element,
//! attribute and line break; model output can only replace identified text leaves.

use std::{borrow::Cow, fmt};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

mod decode;

mod error;

mod parse;

mod protocol;

mod repair;

mod segment;

mod source;

pub use error::*;

pub use parse::*;

pub use protocol::*;

pub use segment::*;

pub use source::*;

pub(crate) use decode::*;

pub(crate) use repair::*;

#[cfg(test)]
mod tests;
