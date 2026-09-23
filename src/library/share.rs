//! Writing and reading share packages.
//!
//! A package carries the book as the canonical EPUB projection plus the three
//! data domains that make a library feel like the reader's own: notes,
//! conversations and whole-book translations.
//!
//! Sections are keyed by **content-unit ordinal**, never by the sender's IDs.
//! Unit IDs are derived from the importing file's content hash and block IDs
//! from the unit ID, so the receiver's own import of the same text produces
//! different identifiers. Every item is therefore re-anchored against the
//! receiving document and dropped — with a count reported back — when it no
//! longer describes that text.

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

/// What one import actually restored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareImportOutcome {
    pub record: BookRecord,
    /// `false` when the same book projection was already in the library and only
    /// the shared sections were applied to it.
    pub added: bool,
    pub notes_imported: usize,
    pub notes_skipped: usize,
    pub chat_threads: usize,
    pub chat_messages: usize,
    pub translations_imported: usize,
    pub translations_skipped: usize,
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
    /// The book goes through the ordinary publish path, so a package whose
    /// projection is already in the library is reported as a duplicate and only
    /// its sections are applied. Sections are written in one transaction after
    /// the book is visible; a failure there leaves the book imported and the
    /// library otherwise untouched.
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
        let source = ImportSource {
            file_name: Some(format!("{}.epub", share_book_stem(&manifest.book.title))),
            bytes: Arc::new(book_epub),
        };
        let imported = self
            .registry
            .import(&source, &ImportLimits::default())
            .context("分享包中的图书无法解析")?;
        let (record, added) = match self.import_parsed(
            imported,
            group_id,
            &format!("分享包《{}》", manifest.book.title),
        )? {
            ImportOutcome::Added(record) => (record, true),
            ImportOutcome::AlreadyExists(record) => (record, false),
        };
        let report = self.apply_shared_sections(&record, &notes, &chats, &translations, stamp)?;
        tracing::info!(
            target: "ngy_share",
            book_id = %record.id,
            added,
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
            added,
            notes_imported: report.notes_imported,
            notes_skipped: report.notes_skipped,
            chat_threads: report.chat_threads,
            chat_messages: report.chat_messages,
            translations_imported: report.translations_imported,
            translations_skipped: report.translations_skipped,
        })
    }

    /// Re-anchors and stores the package sections against the published book.
    fn apply_shared_sections(
        &self,
        record: &BookRecord,
        notes: &[SharedNote],
        chats: &[SharedChatThread],
        translations: &[SharedTranslation],
        stamp: Option<&ShareTranslationStamp>,
    ) -> Result<db::transactions::ShareApplyReport> {
        let document = self.document(&record.id)?;
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
        let mut conn = db::open_conn(&self.db_path)?;
        db::transactions::apply_shared_sections(
            &mut conn,
            &db::transactions::ShareApplyRequest {
                book_id: &record.id,
                document_revision: record.revision,
                units: &units,
                unit_texts: &unit_texts,
                notes,
                chats,
                translations,
                stamp,
                now: now_secs(),
            },
        )
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
        // The final turn is often still streaming its text on the sender's side;
        // an empty thread has nothing to share.
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

fn shared_target_language(translations: &[SharedTranslation]) -> Option<String> {
    let mut languages = translations
        .iter()
        .map(|translation| translation.target_language.clone())
        .collect::<HashSet<_>>();
    if languages.len() == 1 {
        languages.drain().next()
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
