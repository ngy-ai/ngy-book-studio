//! Reproduces an import outside the library window.
//!
//! Runs the format layer, persists through `LibraryStore` into a throwaway data
//! directory, then reopens it — the same path the library window takes, so a
//! failure reported as "导入失败" with an empty console can be reproduced here
//! with the `ngy_import` log stream attached to the terminal. It never touches
//! the real data directory.
//!
//! Usage:
//!   cargo run --example probe_import -- "D:\\tmp\\book\\some book.azw3"
//!   RUST_LOG=warn,ngy_import=debug cargo run --example probe_import -- <path>

use ngy_book_studio::formats::{FormatRegistry, ImportLimits, ImportSource};
use ngy_book_studio::library::{ImportOutcome, LibraryStore};
use std::path::PathBuf;

fn main() {
    // Mirrors `src/main.rs` so the default filter can be verified here.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("warn,ngy_ai=info,ngy_import=debug")
            }),
        )
        .with_file(true)
        .with_line_number(true)
        .try_init();

    let path: PathBuf = match std::env::args().nth(1) {
        Some(value) => PathBuf::from(value),
        None => {
            eprintln!("usage: cargo run --example probe_import -- <book path>");
            return;
        }
    };
    let limits = ImportLimits::default();
    let registry = FormatRegistry::with_builtin_importers();

    let source = match ImportSource::from_path(&path, &limits) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("READ FAILED: {error:#}");
            return;
        }
    };
    println!(
        "bytes={} extension={:?} stem={:?} file_name={:?}",
        source.bytes.len(),
        source.extension(),
        source.stem(),
        source.file_name
    );

    for probe in registry.probe(&source) {
        println!(
            "probe: format={:?} confidence={:?} detail={:?}",
            probe.format, probe.confidence, probe.detail
        );
    }

    match registry.import(&source, &limits) {
        Ok(book) => {
            println!(
                "OK title={:?} authors={:?} language={:?} units={} assets={} toc={} cover={}",
                book.document.title,
                book.document.authors,
                book.document.language,
                book.document.units.len(),
                book.document.assets.len(),
                book.document.toc.len(),
                book.document.cover_asset_id.is_some()
            );
            let mut text_bytes = 0_usize;
            for (index, unit) in book.document.units.iter().enumerate() {
                text_bytes += unit.source.len();
                if index < 5 {
                    println!(
                        "  unit[{}] title={:?} source_len={} blocks={}",
                        index,
                        unit.title,
                        unit.source.len(),
                        unit.document.blocks.len()
                    );
                }
            }
            println!("  total canonical source bytes = {text_bytes}");
            // Retained assets only matter if the markup points at them: a
            // dropped `src` or a leftover Kindle URL means the reader will show
            // a broken picture even though the blob was stored.
            let mut asset_refs = 0_usize;
            let mut leftover_refs = 0_usize;
            for unit in book.document.units.iter() {
                asset_refs += unit.source.matches("ngy-asset:").count();
                leftover_refs += unit.source.matches("kindle:embed:").count();
            }
            println!("  asset references = {asset_refs}, leftover kindle refs = {leftover_refs}");
            // Print a slice of real prose so the content can be eyeballed.
            if let Some(unit) = book
                .document
                .units
                .iter()
                .find(|unit| unit.source.len() > 4000)
                .or_else(|| book.document.units.first())
            {
                let plain = unit
                    .source
                    .replace(['\n', '\t'], " ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                println!(
                    "  sample from {:?}: {}",
                    unit.title,
                    plain.chars().take(300).collect::<String>()
                );
            }
        }
        Err(error) => {
            println!("=== REGISTRY IMPORT FAILED ===");
            println!("error: {error:#}");
            for cause in error.chain() {
                println!("  caused by: {cause}");
            }
            return;
        }
    }

    // The library window goes through `LibraryStore::import`, which also
    // persists blobs and rows. Exercising it here catches store-side failures
    // that the format layer alone would hide.
    let temp = match tempfile::tempdir() {
        Ok(temp) => temp,
        Err(error) => {
            eprintln!("tempdir failed: {error}");
            return;
        }
    };
    let mut library = match LibraryStore::load_from(temp.path().join("library")) {
        Ok(library) => library,
        Err(error) => {
            eprintln!("library open failed: {error:#}");
            return;
        }
    };
    match library.import(&path) {
        Ok(ImportOutcome::Added(record)) => {
            println!("STORE OK: book_id={} title={:?}", record.id, record.title)
        }
        Ok(ImportOutcome::AlreadyExists(record)) => {
            println!("STORE OK (already present): book_id={}", record.id)
        }
        Err(error) => {
            println!("=== STORE IMPORT FAILED ===");
            println!("error: {error:#}");
            for cause in error.chain() {
                println!("  caused by: {cause}");
            }
            return;
        }
    }

    let reopened = match LibraryStore::load_from(temp.path().join("library")) {
        Ok(reopened) => reopened,
        Err(error) => {
            eprintln!("library reopen failed: {error:#}");
            return;
        }
    };
    println!("READ-BACK: books={}", reopened.books().len());
    for book in reopened.books() {
        println!(
            "READ-BACK: title={:?} author={:?} format={} language={:?}",
            book.title, book.author, book.format, book.language
        );
    }
}
