//! Writing and reading share packages.
//!
//! A package carries the book as the canonical EPUB projection plus the three
//! data domains that make a library feel like the reader's own: notes,
//! conversations and whole-book translations.
//!
//! Two things decide how faithfully a package lands.
//!
//! **Whose book is it.** Import first asks whether the sender's own book id is
//! already in the library. Every importer derives that id from the original
//! file's content hash, so a hit means the receiver owns the very same original
//! — the same units, the same blocks, the same ids. Reusing that copy is both
//! the friendly outcome (no duplicate book) and the exact one (citations keep
//! their block, page notes keep their page rule). Only on a miss is the
//! canonical projection published as a new book.
//!
//! **Where each item sits.** Sections are keyed by content-unit ordinal, never
//! by the sender's IDs, because a receiver that had to import the projection
//! derives different unit and block ids from the same text. Every item is
//! re-anchored against the receiving document and dropped — with a count
//! reported back — when it no longer describes that text.

use super::*;

use crate::{
    db,
    formats::{ImportLimits, ImportSource},
    share::{
        self, ShareBookInfo, ShareManifest, ShareSectionSummary, ShareTranslationStamp,
        ShareTranslationSummary, SharedChatCitation, SharedChatMessage, SharedChatThread,
        SharedNote, SharedTranslation, SharedUnitRef,
    },
};

/// What one export wrote, for the completion notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareExportSummary {
    pub path: PathBuf,
    pub bytes: u64,
    pub notes: usize,
    pub chat_threads: usize,
    pub chat_messages: usize,
    pub translations: usize,
    pub target_language: Option<String>,
}

/// How the package's book ended up in the library.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareBookOutcome {
    /// The projection was published as a new book.
    Added,
    /// The library already held the sender's own book, so the shared sections
    /// were applied to that copy and no second copy was created.
    Reused,
    /// The very same projection was already published here by an earlier
    /// import; only the sections were applied.
    AlreadyImported,
}

/// What one import actually restored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareImportOutcome {
    pub record: BookRecord,
    pub book: ShareBookOutcome,
    /// Set when no section at all was applied, with the reason.
    pub sections_note: Option<String>,
    pub notes_imported: usize,
    pub notes_skipped: usize,
    /// Threads created by this import; re-importing only appends new messages.
    pub chat_threads: usize,
    pub chat_messages: usize,
    pub translations_imported: usize,
    pub translations_skipped: usize,
    /// The language the imported machine译文 are stored under.
    pub translations_language: Option<String>,
}

impl LibraryStore {
    /// Writes the share package for one book to `target`.
    ///
    /// Only data that is still current is exported: a note or译文 whose chapter
    /// revision has moved on can no longer be displayed here and could not be
    /// re-anchored by the receiver either.
    pub fn export_share_package(&self, book_id: &str, target: &Path) -> Result<ShareExportSummary> {
        let record = self.book_record(book_id)?;
        let document = self.document(book_id)?;
        let book_epub = self.reader_epub_bytes(book_id)?;
        let conn = db::open_conn(&self.db_path)?;
        let units = document
            .units
            .iter()
            .enumerate()
            .map(|(ordinal, unit)| SharedUnitRef::from_unit(ordinal, unit))
            .collect::<Vec<_>>();
        let ordinal_by_unit = document
            .units
            .iter()
            .enumerate()
            .map(|(ordinal, unit)| (unit.id.clone(), ordinal))
            .collect::<HashMap<_, _>>();
        let notes = export_notes(&conn, &record, &document, &units, &ordinal_by_unit)?;
        let chats = export_chats(&conn, book_id, &units, &ordinal_by_unit)?;
        let translations =
            export_translations(&conn, book_id, &document, &units, &ordinal_by_unit)?;
        let target_language = shared_target_language(&translations);
        let manifest = ShareManifest {
            format: share::SHARE_FORMAT.to_string(),
            version: share::SHARE_VERSION,
            created_at: now_secs(),
            book: ShareBookInfo {
                id: record.id.clone(),
                title: record.title.clone(),
                author: record.author.clone(),
                language: record.language.clone(),
                format: record.format.clone(),
                units: document.units.len(),
            },
            notes: ShareSectionSummary { count: notes.len() },
            chats: ShareSectionSummary { count: chats.len() },
            translations: ShareTranslationSummary {
                count: translations.len(),
                target_language: target_language.clone(),
            },
        };
        let chat_messages = chats.iter().map(|thread| thread.messages.len()).sum();
        let bytes =
            share::write_share_package(&manifest, &book_epub, &notes, &chats, &translations)?;
        crate::export::atomic_write(target, &bytes)?;
        tracing::info!(
            target: "ngy_share",
            book_id = %record.id,
            path = %target.display(),
            notes = notes.len(),
            threads = chats.len(),
            messages = chat_messages,
            translations = translations.len(),
            "分享包已导出"
        );
        Ok(ShareExportSummary {
            path: target.to_path_buf(),
            bytes: bytes.len() as u64,
            notes: notes.len(),
            chat_threads: chats.len(),
            chat_messages,
            translations: translations.len(),
            target_language,
        })
    }

    /// Reads a package from disk and imports it.
    pub fn import_share_package(
        &mut self,
        path: &Path,
        group_id: Option<&str>,
        stamp: Option<&ShareTranslationStamp>,
    ) -> Result<ShareImportOutcome> {
        let bytes =
            fs::read(path).with_context(|| format!("无法读取分享包：{}", path.display()))?;
        let package = share::read_share_package(&bytes)?;
        self.import_share_package_contents(package, group_id, stamp)
    }

    /// Imports an already parsed package.
    ///
    /// The book lands first and the sections are written in one transaction
    /// afterwards, so a failure there leaves the book imported and the library
    /// otherwise untouched.
    pub fn import_share_package_contents(
        &mut self,
        package: share::SharePackage,
        group_id: Option<&str>,
        stamp: Option<&ShareTranslationStamp>,
    ) -> Result<ShareImportOutcome> {
        let share::SharePackage {
            manifest,
            book_epub,
            notes,
            chats,
            translations,
        } = package;
        tracing::info!(
            target: "ngy_share",
            title = %manifest.book.title,
            units = manifest.book.units,
            notes = notes.len(),
            threads = chats.len(),
            translations = translations.len(),
            "分享包：开始导入"
        );
        // The sender's own book id is the content hash of the file they
        // imported. A hit means this library holds that very same file, so its
        // ids are the sender's and reusing it is both friendlier and more exact
        // than publishing a second copy of the same text.
        let owned = {
            let conn = db::open_conn(&self.db_path)?;
            db::books::get(&conn, &manifest.book.id)?
        };
        let (record, book) = match owned {
            Some(record) => {
                tracing::info!(
                    target: "ngy_share",
                    book_id = %record.id,
                    "分享包：复用图书库中已有的同一本图书"
                );
                (record, ShareBookOutcome::Reused)
            }
            None => {
                let source = ImportSource {
                    file_name: Some(format!("{}.epub", share_book_stem(&manifest.book.title))),
                    bytes: Arc::new(book_epub),
                };
                let imported = self
                    .registry
                    .import(&source, &ImportLimits::default())
                    .context("分享包中的图书无法解析")?;
                match self.import_parsed(
                    imported,
                    group_id,
                    &format!("分享包《{}》", manifest.book.title),
                )? {
                    ImportOutcome::Added(record) => (record, ShareBookOutcome::Added),
                    ImportOutcome::AlreadyExists(record) => {
                        (record, ShareBookOutcome::AlreadyImported)
                    }
                }
            }
        };
        // Only a reused book is literally the sender's document, so only there
        // may a citation keep the sender's block-level locator. A reused book
        // also keeps its group: the receiver filed it, not the sender.
        let preserve_locators = book == ShareBookOutcome::Reused;
        let restamp_language = match (
            stamp.and_then(|stamp| stamp.target_language.as_deref()),
            manifest.translations.target_language.as_deref(),
        ) {
            // One shared language can adopt the receiver's reading language.
            // Several cannot: they would all collapse onto one translation
            // scope key and silently overwrite each other.
            (Some(target), Some(_)) => Some(target),
            _ => None,
        };
        let translations_language = restamp_language
            .map(str::to_string)
            .or_else(|| manifest.translations.target_language.clone());
        let (report, sections_note) = self.apply_shared_sections(
            &record,
            &manifest,
            &notes,
            &chats,
            &translations,
            stamp,
            restamp_language,
            preserve_locators,
        )?;
        tracing::info!(
            target: "ngy_share",
            book_id = %record.id,
            reused = preserve_locators,
            notes = report.notes_imported,
            notes_skipped = report.notes_skipped,
            threads = report.chat_threads,
            messages = report.chat_messages,
            translations = report.translations_imported,
            translations_skipped = report.translations_skipped,
            "分享包导入完成"
        );
        Ok(ShareImportOutcome {
            record,
            book,
            sections_note,
            notes_imported: report.notes_imported,
            notes_skipped: report.notes_skipped,
            chat_threads: report.chat_threads,
            chat_messages: report.chat_messages,
            translations_imported: report.translations_imported,
            translations_skipped: report.translations_skipped,
            translations_language,
        })
    }

    /// Re-anchors and stores the package sections against the published book.
    ///
    /// Returns the report plus, when nothing could be applied, the reason.
    #[allow(clippy::too_many_arguments)]
    fn apply_shared_sections(
        &self,
        record: &BookRecord,
        manifest: &ShareManifest,
        notes: &[SharedNote],
        chats: &[SharedChatThread],
        translations: &[SharedTranslation],
        stamp: Option<&ShareTranslationStamp>,
        restamp_language: Option<&str>,
        preserve_locators: bool,
    ) -> Result<(db::transactions::ShareApplyReport, Option<String>)> {
        let document = self.document(&record.id)?;
        // Sections address chapters by position, so a document with a different
        // number of chapters would attach somebody's note to the wrong text.
        // The receiver still gets the book; only the sections are refused.
        if document.units.len() != manifest.book.units {
            let note = format!(
                "分享包按 {} 章打包，图书库中的《{}》现在是 {} 章，章节对不上，其中的笔记、问答与译文都没有导入",
                manifest.book.units,
                record.title,
                document.units.len()
            );
            tracing::warn!(
                target: "ngy_share",
                book_id = %record.id,
                package_units = manifest.book.units,
                local_units = document.units.len(),
                "分享包：章节数不一致，跳过分区"
            );
            return Ok((db::transactions::ShareApplyReport::default(), Some(note)));
        }
        let units = document
            .units
            .iter()
            .enumerate()
            .map(|(ordinal, unit)| db::transactions::ShareUnitTarget {
                ordinal,
                id: unit.id.clone(),
                revision: unit.revision.get(),
                is_page: unit.kind == ContentUnitKind::Page,
            })
            .collect::<Vec<_>>();
        let unit_texts = self.shared_unit_texts(record, notes, &document)?;
        let unit_blocks = self.shared_unit_blocks(record, translations, &document)?;
        let mut conn = db::open_conn(&self.db_path)?;
        let report = db::transactions::apply_shared_sections(
            &mut conn,
            &db::transactions::ShareApplyRequest {
                book_id: &record.id,
                document_revision: record.revision,
                units: &units,
                unit_texts: &unit_texts,
                unit_blocks: &unit_blocks,
                notes,
                chats,
                translations,
                stamp,
                restamp_language,
                preserve_locators,
                now: now_secs(),
            },
        )?;
        Ok((report, None))
    }

    /// Reads the reading projection's body text for every chapter an incoming
    /// note needs. Notes are only accepted where the receiving text still
    /// carries the quote at the recorded offsets, and that comparison is made
    /// against the same chapter bytes the reading window is served.
    fn shared_unit_texts(
        &self,
        record: &BookRecord,
        notes: &[SharedNote],
        document: &crate::document::BookDocument,
    ) -> Result<HashMap<usize, String>> {
        let mut wanted = HashSet::new();
        for note in notes {
            if let Some(unit) = document.units.get(note.unit.ordinal)
                && unit.kind != ContentUnitKind::Page
            {
                wanted.insert(note.unit.ordinal);
            }
        }
        if wanted.is_empty() {
            return Ok(HashMap::new());
        }
        let opened = crate::reader::OpenedBook::open_bytes(self.reader_epub_bytes(&record.id)?)?;
        ensure!(
            opened.spine.len() == document.units.len(),
            "阅读章节与图书结构不一致，无法还原分享笔记"
        );
        let mut texts = HashMap::with_capacity(wanted.len());
        for ordinal in wanted {
            let href = &opened.spine[ordinal].href;
            let response = crate::reader::load_resource(&opened.epub, href)?;
            let html = std::str::from_utf8(&response.bytes).context("无法解码分享笔记所在章节")?;
            texts.insert(ordinal, crate::annotations::reader_body_text(html));
        }
        Ok(texts)
    }

    /// Derives the receiving document's own translation-block texts, numbered
    /// exactly as its translation job numbers them: an EPUB source counts the
    /// reading chapters, every other source its canonical HTML. The importer
    /// accepts a shared译文 only where the block at its recorded position still
    /// carries its source text, so this must be the same derivation the block
    /// ids on this side describe.
    fn shared_unit_blocks(
        &self,
        record: &BookRecord,
        translations: &[SharedTranslation],
        document: &crate::document::BookDocument,
    ) -> Result<HashMap<usize, Vec<String>>> {
        let wanted = translations
            .iter()
            .filter(|translation| document.units.get(translation.unit.ordinal).is_some())
            .map(|translation| translation.unit.ordinal)
            .collect::<HashSet<_>>();
        let mut blocks = HashMap::with_capacity(wanted.len());
        if wanted.is_empty() {
            return Ok(blocks);
        }
        if record.format == "epub" {
            let opened =
                crate::reader::OpenedBook::open_bytes(self.reader_epub_bytes(&record.id)?)?;
            ensure!(
                opened.spine.len() == document.units.len(),
                "阅读章节与图书结构不一致，无法还原分享译文"
            );
            for ordinal in wanted {
                let href = &opened.spine[ordinal].href;
                let response = crate::reader::load_resource(&opened.epub, href)?;
                let html =
                    std::str::from_utf8(&response.bytes).context("无法解码分享译文所在章节")?;
                let texts = crate::markup::translation_blocks_from_document_html(html)?
                    .into_iter()
                    .map(|block| block.text)
                    .collect();
                blocks.insert(ordinal, texts);
            }
        } else {
            for ordinal in wanted {
                let source = crate::markup::serialize_source(&document.units[ordinal].document)?;
                let texts = crate::markup::translation_blocks_from_html(&source)?
                    .into_iter()
                    .map(|block| block.text)
                    .collect();
                blocks.insert(ordinal, texts);
            }
        }
        Ok(blocks)
    }
}

fn export_notes(
    conn: &rusqlite::Connection,
    record: &BookRecord,
    document: &crate::document::BookDocument,
    units: &[SharedUnitRef],
    ordinal_by_unit: &HashMap<String, usize>,
) -> Result<Vec<SharedNote>> {
    let mut notes = Vec::new();
    for note in db::annotations::list(conn, &record.id, None)? {
        let Some(&ordinal) = ordinal_by_unit.get(&note.content_unit_id) else {
            continue;
        };
        // A note whose chapter moved on cannot be shown here and could not be
        // re-anchored on the receiving side either.
        if note.document_revision != record.revision
            || document.units[ordinal].revision.get() != note.unit_revision
        {
            continue;
        }
        notes.push(SharedNote {
            unit: units[ordinal].clone(),
            kind: note.kind.as_str().to_string(),
            quote: note.anchor.quote,
            start: note.anchor.start,
            end: note.anchor.end,
            comment: note.comment,
            created_at: note.created_at,
            updated_at: note.updated_at,
        });
    }
    Ok(notes)
}

fn export_chats(
    conn: &rusqlite::Connection,
    book_id: &str,
    units: &[SharedUnitRef],
    ordinal_by_unit: &HashMap<String, usize>,
) -> Result<Vec<SharedChatThread>> {
    let mut threads = Vec::new();
    for thread in db::chat_threads::list_for_book(conn, Some(book_id))? {
        let messages = db::chat_messages::list_for_thread(conn, &thread.id)?;
        let ordinal_by_message = messages
            .iter()
            .map(|message| (message.id.clone(), message.ordinal))
            .collect::<HashMap<_, _>>();
        let mut shared_messages = Vec::new();
        for message in &messages {
            // Only the conversation the reader can see travels; internal system
            // and tool turns stay behind.
            if !matches!(message.role.as_str(), "user" | "assistant") {
                continue;
            }
            let mut citations = Vec::new();
            for citation in db::chat_citations::list_for_message(conn, &message.id)? {
                let unit = citation
                    .content_unit_id
                    .as_deref()
                    .and_then(|unit_id| ordinal_by_unit.get(unit_id))
                    .map(|ordinal| units[*ordinal].clone());
                if citation.source_kind != "web" && unit.is_none() {
                    continue;
                }
                citations.push(SharedChatCitation {
                    unit,
                    source_kind: citation.source_kind,
                    quote: citation.quote,
                    url: citation.url,
                    source_title: citation.source_title,
                    locator_json: citation.locator_json,
                });
            }
            shared_messages.push(SharedChatMessage {
                ordinal: message.ordinal,
                parent_ordinal: message
                    .parent_id
                    .as_deref()
                    .and_then(|parent| ordinal_by_message.get(parent).copied()),
                role: message.role.clone(),
                content: message.content.clone(),
                model: message.model.clone(),
                created_at: message.created_at,
                citations,
            });
        }
        // A thread whose turns were all filtered out has nothing to share.
        if shared_messages.is_empty() {
            continue;
        }
        threads.push(SharedChatThread {
            title: thread.title,
            window_kind: thread.window_kind,
            created_at: thread.created_at,
            updated_at: thread.updated_at,
            messages: shared_messages,
        });
    }
    Ok(threads)
}

fn export_translations(
    conn: &rusqlite::Connection,
    book_id: &str,
    document: &crate::document::BookDocument,
    units: &[SharedUnitRef],
    ordinal_by_unit: &HashMap<String, usize>,
) -> Result<Vec<SharedTranslation>> {
    let mut translations = Vec::new();
    for row in db::translations::list_for_book(conn, book_id)? {
        let Some(&ordinal) = ordinal_by_unit.get(&row.content_unit_id) else {
            continue;
        };
        if row.document_revision != document.revision.get()
            || row.unit_revision != document.units[ordinal].revision.get()
        {
            continue;
        }
        let Some(block_index) = share::block_index_of(&row.block_id, &row.content_unit_id) else {
            continue;
        };
        translations.push(SharedTranslation {
            unit: units[ordinal].clone(),
            block_index,
            ordinal: row.ordinal as usize,
            target_language: row.target_language,
            source_language: row.source_language,
            model: row.model,
            source_text: row.source_text,
            translated_text: row.translated_text,
            manual_text: row.manual_text,
        });
    }
    Ok(translations)
}

/// The single language a package's译文 share, or `None` when it carries several
/// (and `None` for an empty section).
fn shared_target_language(translations: &[SharedTranslation]) -> Option<String> {
    let languages = translations
        .iter()
        .map(|translation| translation.target_language.clone())
        .collect::<HashSet<_>>();
    if languages.len() == 1 {
        languages.into_iter().next()
    } else {
        None
    }
}

/// A file name for the re-imported projection, derived from the book title so
/// the library reports a recognizable source name.
fn share_book_stem(title: &str) -> String {
    let stem = title
        .chars()
        .filter(|ch| !matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .filter(|ch| !ch.is_control())
        .take(80)
        .collect::<String>();
    let stem = stem.trim().to_string();
    if stem.is_empty() {
        "shared-book".to_string()
    } else {
        stem
    }
}
