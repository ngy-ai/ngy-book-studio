use anyhow::{Context as _, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use crate::document::{DocumentLocator, SourceLocator};

/// Identifies SQLite files owned by this application (ASCII "NGYB").
mod column;

mod create;

mod current;

mod inspect;

mod normalize;

mod specs;

mod tables;

mod validate;

pub(crate) use column::*;

pub(crate) use create::*;

pub(crate) use current::*;

pub(crate) use inspect::*;

pub(crate) use normalize::*;

pub(crate) use specs::*;

pub(crate) use tables::*;

pub(crate) use validate::*;

#[cfg(test)]
mod tests;
/// Identifies SQLite files owned by this application (ASCII "NGYB").
pub(crate) const APPLICATION_ID: i64 = 0x4E47_5942;
/// Development schemas are deliberately rebuilt instead of migrated.
pub(crate) const SCHEMA_VERSION: i64 = 14;
