use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use ngy_book_studio::{
    ai::credentials::MemoryCredentialStore,
    ai::job_diagnostics::JobLogErrorKind,
    ai::translation::TranslationSegment,
    library::ImportOutcome,
    services::{
        AppServices, BackgroundJobAction, BackgroundJobSnapshot, BackgroundJobStatus,
        ProviderSettings, TranslatedBlock,
    },
};
use serde_json::{Value, json};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

mod endpoint;

mod fixture;

mod a_test;

mod b_test;

mod c_test;

pub(crate) use endpoint::*;

pub(crate) use fixture::*;
