//! Reproduces what the reading WebView actually receives, outside the GUI.
//!
//! Imports a book into a throwaway library, generates the reader EPUB exactly
//! the way `LibraryStore::reader_epub_bytes` does, then serves each spine
//! chapter through `reader::load_resource` — the same function the custom
//! `epubreader://` protocol handler calls. Any named character reference that
//! XML does not predefine (e.g. `&nbsp;`) is reported with its line/column, so
//! a "Entity 'nbsp' not defined" WebView page can be traced to the byte that
//! caused it.
//!
//! Usage:
//!   cargo run --example probe_reader -- "D:\\tmp\\book\\some book.azw3"
//!   RUST_LOG=warn,ngy_reader=debug cargo run --example probe_reader -- <path>

use ngy_book_studio::library::{ImportOutcome, LibraryStore};
use ngy_book_studio::reader::{OpenedBook, load_resource};
use std::path::PathBuf;

/// Named references XML predefines; every other `&name;` needs a DTD and
/// therefore breaks a document served as `application/xhtml+xml`.
const XML_PREDEFINED: [&str; 5] = ["amp", "lt", "gt", "quot", "apos"];

struct EntityHit {
    name: String,
    line: usize,
    column: usize,
}

fn scan_undefined_entities(text: &str) -> Vec<EntityHit> {
    let bytes = text.as_bytes();
    let mut hits = Vec::new();
    let mut line = 1_usize;
    let mut line_start = 0_usize;
    let mut cursor = 0_usize;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\n' => {
                line += 1;
                line_start = cursor + 1;
                cursor += 1;
            }
            b'&' => {
                let name_start = cursor + 1;
                let mut end = name_start;
                while bytes
                    .get(end)
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                {
                    end += 1;
                }
                if end > name_start && bytes.get(end) == Some(&b';') {
                    let name = &text[name_start..end];
                    // Numeric references (`&#160;`, `&#xA0;`) are always legal.
                    if !name.starts_with('#') && !XML_PREDEFINED.contains(&name) {
                        hits.push(EntityHit {
                            name: name.to_string(),
                            line,
                            column: cursor - line_start + 1,
                        });
                    }
                    cursor = end + 1;
                } else {
                    cursor += 1;
                }
            }
            _ => cursor += 1,
        }
    }
    hits
}

fn main() {
    // Mirrors `src/main.rs` so the default filter can be verified here.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(
                    "warn,ngy_ai=info,ngy_import=debug,ngy_reader=debug",
                )
            }),
        )
        .with_file(true)
        .with_line_number(true)
        .try_init();

    let path: PathBuf = match std::env::args().nth(1) {
        Some(value) => PathBuf::from(value),
        None => {
            eprintln!("usage: cargo run --example probe_reader -- <book path>");
            return;
        }
    };

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
    let book_id = match library.import(&path) {
        Ok(ImportOutcome::Added(record)) | Ok(ImportOutcome::AlreadyExists(record)) => {
            println!("IMPORT OK: book_id={} format={}", record.id, record.format);
            record.id
        }
        Err(error) => {
            println!("=== IMPORT FAILED ===");
            println!("error: {error:#}");
            return;
        }
    };

    // Inspect the stored canonical source for both spellings of a non-breaking
    // space: the literal character and the HTML named reference.
    if let Ok(document) = library.document(&book_id) {
        for (index, unit) in document.units.iter().take(3).enumerate() {
            let source = &unit.source;
            println!(
                "STORED unit[{index}] title={:?} source_bytes={} literal_nbsp={} named_nbsp={} named_amp={}",
                unit.title,
                source.len(),
                source.matches('\u{a0}').count(),
                source.matches("&nbsp;").count(),
                source.matches("&amp;").count()
            );
        }
    }

    let epub_bytes = match library.reader_epub_bytes(&book_id) {
        Ok(bytes) => bytes,
        Err(error) => {
            println!("=== READER EPUB FAILED ===");
            println!("error: {error:#}");
            return;
        }
    };
    println!("READER EPUB: {} bytes", epub_bytes.len());

    let opened = match OpenedBook::open_bytes(epub_bytes) {
        Ok(opened) => opened,
        Err(error) => {
            println!("=== EPUB OPEN FAILED ===");
            println!("error: {error:#}");
            return;
        }
    };
    println!("SPINE: {} chapters", opened.spine.len());

    let mut broken = 0_usize;
    for (index, item) in opened.spine.iter().enumerate() {
        let request_path = if item.href.starts_with('/') {
            item.href.clone()
        } else {
            format!("/{}", item.href)
        };
        let response = match load_resource(&opened.epub, &request_path) {
            Ok(response) => response,
            Err(error) => {
                println!("  spine[{index}] {} -> LOAD FAILED: {error:#}", item.href);
                continue;
            }
        };
        let text = String::from_utf8_lossy(&response.bytes);
        let hits = scan_undefined_entities(&text);
        let literal_nbsp = text.matches('\u{a0}').count();
        let first = hits.first();
        println!(
            "  spine[{index}] href={:?} mime={:?} bytes={} literal_nbsp={} undefined_entities={}",
            item.href,
            response.mime,
            response.bytes.len(),
            literal_nbsp,
            hits.len()
        );
        if let Some(hit) = first {
            broken += 1;
            println!(
                "    BREAKS XML: &{}; at line {} column {} (first of {})",
                hit.name,
                hit.line,
                hit.column,
                hits.len()
            );
            // Print the offending line with a caret so the byte is visible.
            if let Some(line_text) = text.lines().nth(hit.line.saturating_sub(1)) {
                let start = hit.column.saturating_sub(60);
                let snippet: String = line_text
                    .chars()
                    .skip(start)
                    .take(140)
                    .collect::<String>()
                    .replace('\u{a0}', "\u{2423}");
                println!("    line {}: {}", hit.line, snippet);
            }
            let mut names = std::collections::BTreeMap::<String, usize>::new();
            for hit in &hits {
                *names.entry(hit.name.clone()).or_default() += 1;
            }
            println!("    entity histogram: {names:?}");
        }
        if index >= 19 {
            println!("  ... (stopping after the first 20 chapters)");
            break;
        }
    }

    println!("SUMMARY: {broken} chapter(s) would fail XML parsing");
}
