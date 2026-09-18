use std::{
    collections::{HashMap, HashSet},
    fmt, fs,
    future::Future,
    ops::Range,
    path::{Path, PathBuf},
    pin::Pin,
    str::FromStr,
    sync::{Arc, Mutex as StdMutex, OnceLock, Weak},
};

use anyhow::{Context as _, Result, ensure};
use object_store::{
    GetOptions, GetRange, ObjectStore as _, ObjectStoreExt as _, PutMode, PutOptions,
    local::LocalFileSystem, path::Path as ObjectPath,
};

pub(crate) const HASH_ALGORITHM: &str = "blake3";
mod key;

mod local;

mod local_io;

mod local_store;

mod local_validate;

mod lock;

mod store;

mod util;

pub use key::*;

pub use local::*;

pub use store::*;

pub(crate) use lock::*;

pub(crate) use util::*;

#[cfg(test)]
mod tests;
pub(crate) const DIGEST_HEX_LEN: usize = 64;
pub(crate) const SHARD_HEX_LEN: usize = 2;

pub(crate) type PublicationGate = tokio::sync::Mutex<()>;

pub(crate) static PUBLICATION_GATES: OnceLock<StdMutex<HashMap<PathBuf, Weak<PublicationGate>>>> =
    OnceLock::new();
